import assert from 'node:assert/strict';
import { test } from 'node:test';
import binaryen from 'binaryen';
import { normalizeTailCalls, TAIL_CALL_POLICY } from '../build-tools/tail-call-compat.mjs';

function compile(wat, extraFeatures = 0) {
  const module = binaryen.parseText(wat);
  try {
    module.setFeatures(
      binaryen.Features.TailCall |
        binaryen.Features.SIMD128 |
        binaryen.Features.Multivalue |
        binaryen.Features.ReferenceTypes |
        extraFeatures
    );
    assert.equal(module.validate(), 1);
    return module.emitBinary();
  } finally {
    module.dispose();
  }
}
function transformed(wat, imports = {}) {
  const { bytes, compatibility } = normalizeTailCalls(compile(wat));
  assert.equal(WebAssembly.validate(bytes), true);
  assert.equal(compatibility.policy, TAIL_CALL_POLICY);
  const emitted = normalizeTailCalls(bytes).compatibility;
  assert.equal(emitted.imported, 0);
  assert.equal(emitted.indirect, 0);
  return {
    exports: new WebAssembly.Instance(new WebAssembly.Module(bytes), imports).exports,
    compatibility,
    bytes,
  };
}

test('host tail calls preserve void, integer, float, and multiple results', () => {
  const calls = [];
  const r = transformed(
    `(module
    (import "host" "void" (func $void (param i32)))
    (import "host" "one" (func $one (param i64) (result i64)))
    (import "host" "float" (func $float (result f64)))
    (import "host" "multi" (func $multi (result i32 i64)))
    (func (export "void") (param i32) local.get 0 return_call $void)
    (func (export "one") (param i64) (result i64) local.get 0 return_call $one)
    (func (export "float") (result f64) return_call $float)
    (func (export "multi") (result i32 i64) return_call $multi))`,
    {
      host: {
        void: (x) => calls.push(x),
        one: (x) => x + 1n,
        float: () => 2.5,
        multi: () => [7, 8n],
      },
    }
  );
  assert.equal(r.exports.void(42), undefined);
  assert.deepEqual(calls, [42]);
  assert.equal(r.exports.one(2n ** 50n), 2n ** 50n + 1n);
  assert.equal(r.exports.float(), 2.5);
  assert.deepEqual(r.exports.multi(), [7, 8n]);
  assert.equal(r.compatibility.imported, 4);
});

test('a lowered tail call still returns from nested control flow', () => {
  const r = transformed(
    `(module
    (import "h" "f" (func $f (result i32)))
    (func (export "f") (param i32) (result i32)
      block
        local.get 0
        if
          return_call $f
        end
      end
      i32.const 99))`,
    { h: { f: () => 42 } }
  );
  assert.equal(r.exports.f(1), 42);
  assert.equal(r.exports.f(0), 99);
});

test('host exceptions propagate with their exact identity', () => {
  const sentinel = new Error('host exception');
  const r = transformed(
    `(module
    (import "h" "f" (func $f))
    (func (export "f") return_call $f))`,
    {
      h: {
        f: () => {
          throw sentinel;
        },
      },
    }
  );
  assert.throws(
    () => r.exports.f(),
    (error) => error === sentinel
  );
});

test('reference null checks preserve both null and non-null values', () => {
  const r = transformed(`(module
    (func (export "is_null") (param externref) (result i32)
      local.get 0 ref.is_null))`);
  assert.equal(r.exports.is_null(null), 1);
  assert.equal(r.exports.is_null({}), 0);
});

test('indirect calls retain arguments, table index, results, and evaluation order', () => {
  const order = [];
  const r = transformed(
    `(module
    (type $f (func (param i32 i32) (result i32)))
    (import "h" "arg" (func $arg (param i32) (result i32)))
    (table 2 funcref)
    (func $sum (type $f) local.get 0 local.get 1 i32.add)
    (elem (i32.const 1) $sum)
    (func (export "f") (result i32)
      i32.const 3 call $arg
      i32.const 4 call $arg
      i32.const 1 call $arg
      return_call_indirect (type $f)))`,
    {
      h: {
        arg: (n) => {
          order.push(n);
          return n;
        },
      },
    }
  );
  assert.equal(r.exports.f(), 7);
  assert.deepEqual(order, [3, 4, 1]);
  assert.equal(r.compatibility.indirect, 1);
});

test('indirect void and multi-value results survive cross-instance calls', () => {
  const target = new WebAssembly.Instance(
    new WebAssembly.Module(
      compile(`(module
    (func (export "answer") (result i32 i64) i32.const 42 i64.const 9))`)
    )
  ).exports.answer;
  const r = transformed(
    `(module
    (type $void (func))
    (type $pair (func (result i32 i64)))
    (import "h" "pair" (func $pair (type $pair)))
    (import "h" "done" (func $done))
    (table 2 funcref)
    (elem (i32.const 0) $done $pair)
    (func (export "void") i32.const 0 return_call_indirect (type $void))
    (func (export "pair") (result i32 i64) i32.const 1 return_call_indirect (type $pair)))`,
    {
      h: { pair: target, done: () => {} },
    }
  );
  assert.equal(r.exports.void(), undefined);
  assert.deepEqual(r.exports.pair(), [42, 9n]);
  assert.equal(r.compatibility.indirect, 2);
});

test('indirect calls preserve null, out-of-bounds, and signature traps', () => {
  const r = transformed(`(module
    (type $value (func (result i32)))
    (table 2 funcref)
    (func $wrong)
    (elem (i32.const 1) $wrong)
    (func (export "f") (param i32) (result i32)
      local.get 0 return_call_indirect (type $value)))`);
  for (const index of [0, 1, 2]) assert.throws(() => r.exports.f(index), WebAssembly.RuntimeError);
});

test('indirect calls preserve SIMD vector arguments and results', () => {
  const r = transformed(`(module
    (type $vector (func (param v128) (result v128)))
    (table 1 funcref)
    (func $identity (type $vector) local.get 0)
    (elem (i32.const 0) $identity)
    (func $dispatch (param v128) (result v128)
      local.get 0 i32.const 0 return_call_indirect (type $vector))
    (func (export "f") (result i32)
      v128.const i32x4 42 0 0 0 call $dispatch i32x4.extract_lane 0))`);
  assert.equal(r.exports.f(), 42);
  assert.equal(r.compatibility.indirect, 1);
});

test('the upstream two-instance direct-tail reproducer uses an ordinary boundary call', () => {
  const source = compile(`(module
    (import "other" "g" (func $g (result i32)))
    (func (export "f") (result i32) return_call $g)
    (func (export "g") (result i32) i32.const 42))`);
  const { bytes, compatibility } = normalizeTailCalls(source);
  const first = new WebAssembly.Instance(new WebAssembly.Module(bytes), { other: { g: () => 0 } });
  const second = new WebAssembly.Instance(new WebAssembly.Module(bytes), {
    other: { g: first.exports.g },
  });
  for (let i = 0; i < 2000; i++) assert.equal(second.exports.f(), 42);
  assert.equal(compatibility.imported, 1);
});

test('internal SIMD and direct tail recursion remain enabled', () => {
  const r = transformed(`(module
    (func $count (param i32) (result i32)
      local.get 0 i32.eqz
      if (result i32)
        v128.const i32x4 42 0 0 0 i32x4.extract_lane 0
      else
        local.get 0 i32.const 1 i32.sub return_call $count
      end)
    (func (export "run") (result i32) i32.const 200000 return_call $count))`);
  assert.equal(r.exports.run(), 42);
  assert.equal(r.compatibility.internal, 2);
  assert.equal(r.compatibility.imported + r.compatibility.indirect, 0);
  const second = normalizeTailCalls(r.bytes);
  assert.equal(second.compatibility.internal, 2);
  assert.equal(second.compatibility.imported + second.compatibility.indirect, 0);
});

test('future Wasm exception handlers fail closed', () => {
  const bytes = compile(
    `(module
    (import "h" "f" (func $f))
    (tag $e)
    (func (export "f") (try (do (return_call $f)) (catch $e))))`,
    binaryen.Features.ExceptionHandling
  );
  assert.throws(() => normalizeTailCalls(bytes), /Invalid Wasm|Unsupported Wasm/);
});
