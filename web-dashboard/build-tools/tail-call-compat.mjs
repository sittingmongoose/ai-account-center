import fs from 'node:fs';
import { pathToFileURL } from 'node:url';
import binaryen from 'binaryen';

export const TAIL_CALL_POLICY = 'webkit-ipint-boundaries-v1';
const FEATURES = [
  'MutableGlobals',
  'NontrappingFPToInt',
  'SIMD128',
  'BulkMemory',
  'BulkMemoryOpt',
  'SignExt',
  'TailCall',
  'ReferenceTypes',
  'Multivalue',
  'ExtendedConst',
].reduce((bits, name) => bits | binaryen.Features[name], binaryen.Features.MVP);

// iOS 27's JIT cage can reject IPInt's return-address authentication when a
// tail call crosses an instance/host boundary (WebKit 325447 / PR 76671).
// Keep direct calls within this module as tail calls. Imports and indirect
// calls use an ordinary call followed by an explicit function return instead.
// This deliberately gives those indirect paths an extra stack frame; it is
// suitable for this Rust dashboard, not a general proper-tail-call polyfill.
// Run after wasm-bindgen and any optimizer, before hashing or compression.
const childSpecs = new Map();
function children(kind, fields = [], list = null) {
  childSpecs.set(binaryen[`${kind}Id`], { api: binaryen[kind], fields, list });
}
children('Block', [], ['getNumChildren', 'getChildAt', 'setChildAt']);
children('If', ['Condition', 'IfTrue', 'IfFalse']);
children('Loop', ['Body']);
children('Break', ['Condition', 'Value']);
children('Switch', ['Condition', 'Value']);
children('Call', [], ['getNumOperands', 'getOperandAt', 'setOperandAt']);
children('CallIndirect', ['Target'], ['getNumOperands', 'getOperandAt', 'setOperandAt']);
children('LocalSet', ['Value']);
children('GlobalSet', ['Value']);
children('TableGet', ['Index']);
children('TableSet', ['Index', 'Value']);
children('TableGrow', ['Value', 'Delta']);
children('Load', ['Ptr']);
children('Store', ['Ptr', 'Value']);
children('Unary', ['Value']);
children('Binary', ['Left', 'Right']);
children('Select', ['IfTrue', 'IfFalse', 'Condition']);
children('Drop', ['Value']);
children('Return', ['Value']);
children('MemoryGrow', ['Delta']);
children('SIMDExtract', ['Vec']);
children('SIMDReplace', ['Vec', 'Value']);
children('SIMDShuffle', ['Left', 'Right']);
children('SIMDTernary', ['A', 'B', 'C']);
children('SIMDShift', ['Vec', 'Shift']);
children('SIMDLoad', ['Ptr']);
children('SIMDLoadStoreLane', ['Ptr', 'Vec']);
children('MemoryInit', ['Dest', 'Offset', 'Size']);
children('MemoryCopy', ['Dest', 'Source', 'Size']);
children('MemoryFill', ['Dest', 'Value', 'Size']);
children('RefIsNull', ['Value']);
children('RefAs', ['Value']);
children('RefEq', ['Left', 'Right']);
for (const kind of [
  'Const',
  'Nop',
  'Unreachable',
  'LocalGet',
  'GlobalGet',
  'TableSize',
  'MemorySize',
  'DataDrop',
  'RefNull',
  'RefFunc',
])
  children(kind);

// Fail closed if code generation changes. Lowering a tail call within a Wasm
// exception handler can change which handler catches an exception. Tail refs
// and other new instructions need a deliberate review before being supported.
const unsupported = new Set(
  ['Try', 'TryTable', 'Throw', 'ThrowRef', 'Rethrow', 'Pop', 'CallRef']
    .map((kind) => binaryen[`${kind}Id`])
    .filter((id) => id !== undefined)
);

function childSlots(expression, spec) {
  const slots = [];
  for (const field of spec.fields) {
    const child = spec.api[`get${field}`](expression);
    if (child) slots.push({ expression: child, set: spec.api[`set${field}`], parent: expression });
  }
  if (spec.list) {
    const [count, get, set] = spec.list;
    for (let index = 0; index < spec.api[count](expression); index++) {
      slots.push({
        expression: spec.api[get](expression, index),
        set: spec.api[set],
        parent: expression,
        index,
      });
    }
  }
  return slots;
}

/** Decode, narrowly normalize, validate, and re-encode a complete Wasm module. */
export function normalizeTailCalls(bytes) {
  const module = binaryen.readBinary(bytes);
  try {
    module.setFeatures(FEATURES);
    if (!module.validate())
      throw new Error('Invalid Wasm before tail-call compatibility processing.');
    const functions = new Map();
    for (let index = 0; index < module.getNumFunctions(); index++) {
      const ref = module.getFunctionByIndex(index);
      const info = binaryen.getFunctionInfo(ref);
      functions.set(info.name, { ref, ...info });
    }
    const counts = { imported: 0, indirect: 0, internal: 0 };
    for (const func of functions.values()) {
      if (!func.body) continue;
      // Iterative postorder avoids a JS recursion limit on deeply nested Slint
      // control flow. Child setters preserve every other instruction/operand.
      const stack = [{ expression: func.body, root: true }];
      while (stack.length) {
        const task = stack.pop();
        const expression = task.expression;
        const id = binaryen.getExpressionId(expression);
        const spec = childSpecs.get(id);
        if (unsupported.has(id) || !spec) {
          throw new Error(
            `Unsupported Wasm expression ${id} in tail-call compatibility processing.`
          );
        }
        if (!task.visited) {
          const slots = childSlots(expression, spec);
          if (slots.length) {
            stack.push({ ...task, visited: true });
            for (const slot of slots) stack.push(slot);
            continue;
          }
        }
        let call;
        let results;
        if (id === binaryen.CallId && binaryen.Call.isReturn(expression)) {
          const target = binaryen.Call.getTarget(expression);
          const callee = functions.get(target);
          if (!callee) throw new Error('Missing tail-call target.');
          if (callee.body) {
            counts.internal++;
            continue;
          }
          results = callee.results;
          call = module.call(target, binaryen.Call.getOperands(expression), results);
          counts.imported++;
        } else if (id === binaryen.CallIndirectId && binaryen.CallIndirect.isReturn(expression)) {
          results = binaryen.CallIndirect.getResults(expression);
          call = module.call_indirect(
            binaryen.CallIndirect.getTable(expression),
            binaryen.CallIndirect.getTarget(expression),
            binaryen.CallIndirect.getOperands(expression),
            binaryen.CallIndirect.getParams(expression),
            results
          );
          counts.indirect++;
        } else continue;
        // A void return cannot carry a value, including a void-valued call.
        const replacement =
          results === binaryen.none
            ? module.block(null, [call, module.return()], binaryen.unreachable)
            : module.return(call);
        if (task.root) binaryen.Function.setBody(func.ref, replacement);
        else if (task.index === undefined) task.set(task.parent, replacement);
        else task.set(task.parent, task.index, replacement);
      }
    }
    if (!module.validate())
      throw new Error('Invalid Wasm after tail-call compatibility processing.');
    return { bytes: module.emitBinary(), compatibility: { policy: TAIL_CALL_POLICY, ...counts } };
  } finally {
    module.dispose();
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const input = process.argv[2];
    if (!input) throw new Error('A Wasm input path is required.');
    const result = normalizeTailCalls(fs.readFileSync(input));
    fs.writeFileSync(input, result.bytes);
    console.log(JSON.stringify(result.compatibility));
  } catch (error) {
    console.error(`[X] ${error instanceof Error ? error.message : 'Tail-call processing failed.'}`);
    process.exitCode = 1;
  }
}
