# Runtime dependencies

The AI Account Center source and its original-fork notices retain their existing
license. These separately installed Python libraries retain their own notices:

- [pyte 0.8.2](https://pypi.org/project/pyte/0.8.2/) by Sergei Lebedev is licensed
  under LGPL version 3. Its unmodified source is available from
  [the upstream repository](https://github.com/selectel/pyte) and its release.
- [wcwidth 0.9.1](https://pypi.org/project/wcwidth/0.9.1/) by Jeff Quast is licensed
  under MIT. Its package retains its copyright and license files.

The installer uses a separate versioned virtual environment and hash-checked
official wheels. It does not copy these dependencies into the application's
TypeScript source, modify the system Python installation, or strip their notices.
