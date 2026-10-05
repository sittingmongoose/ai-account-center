# Runtime dependencies

The AI Account Center source and its original-fork notices retain their existing
license. These separately installed Python libraries retain their own notices:

- [pyte 0.8.2](https://pypi.org/project/pyte/0.8.2/) by Sergei Lebedev is licensed
  under LGPL version 3. Its unmodified source is available from
  [the upstream repository](https://github.com/selectel/pyte) and its release.
- [wcwidth 0.9.1](https://pypi.org/project/wcwidth/0.9.1/) by Jeff Quast is licensed
  under MIT. Its package retains its copyright and license files.

The installer fetches the hash-checked official wheels into the versioned
runtime bundle's own `parser/` directory, next to the bundle's private virtual
environment. pyte is pure Python; wcwidth installs as a CPython stable-ABI
(`abi3`, 3.10 and later) wheel. Both load under later CPython 3 minor versions,
so that directory does not depend on the system Python minor version and an
Ubuntu release upgrade of Python does not orphan it. Each package keeps its `.dist-info` metadata and license files
there. The installer does not copy these dependencies into the application's
TypeScript source, modify the system Python installation, or strip their notices.
