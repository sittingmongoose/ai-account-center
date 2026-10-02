"""Offline-only narrow kitty query refinement of the immutable 37-fixture parser.

No terminal/body/image/path bytes are exported or decoded. Only exact `a=q`
requests may receive a fixed unsupported error; drawing/data commands still fail.
Primary: https://sw.kovidgoyal.net/kitty/graphics-protocol/
"""
import hashlib
from pathlib import Path
import re
from native_header import NativeHeader as FrozenNativeHeader

BASE_SHA256 = '015eda31b55df35a71b6ed3ab436aa9d33f180614699ea903247267a73c4b1e7'
if hashlib.sha256(Path(__file__).with_name('native_header.py').read_bytes()).hexdigest() != BASE_SHA256:
    raise RuntimeError('frozen-terminal-observer-binding-changed')


class NativeHeader(FrozenNativeHeader):
    QUERY_KEYS = {'a', 'i', 's', 'v', 't', 'f', 'q'}
    NUMERIC_KEYS = {'i', 's', 'v', 'f', 'q'}
    ACTIONS = {'t', 'T', 'p', 'q', 'd', 'f', 'a', 'c'}

    @classmethod
    def _safe_graphics_shape(cls, payload):
        # Whitelist opcode/numeric metadata only. Never echo unknown values,
        # header/body strings, image bytes, filenames or base64 contents.
        header, separator, body = payload.partition(';')
        fields = header[1:].split(',') if header.startswith('G') else []
        numeric, action, names = {}, None, set()
        for item in fields[:16]:
            key, equal, value = item.partition('=')
            if equal and key in cls.QUERY_KEYS:names.add(key)
            if key == 'a' and value in cls.ACTIONS:action = value
            if key in cls.NUMERIC_KEYS and re.fullmatch(r'\d{1,10}', value):
                number = int(value)
                if number <= 4294967295:numeric[key] = number
        return {'command': 'G' if payload.startswith('G') else None,
                'action': action, 'numericControlFields': numeric,
                'knownControlFieldNames': sorted(names),
                'controlFieldCount': min(len(fields), 17),
                'bodyLength': min(len(body), 4097) if separator else 0}

    def _reject_graphics(self, payload):
        shape = self._safe_graphics_shape(payload)
        try:self._reject('APC')
        except ValueError:
            self.unsupported_control['graphics'] = shape
            raise

    def _string(self, family, payload, terminator):
        if family != '_':return super()._string(family, payload, terminator)
        self.control_families.add('APC')
        if not payload.startswith('G') or terminator != '\x1b\\':self._reject_graphics(payload)
        header, separator, body = payload[1:].partition(';')
        if not separator or len(header) > 128 or len(body.encode('utf-8')) > 4096:
            self._reject_graphics(payload)
        fields = header.split(',')
        if not 2 <= len(fields) <= len(self.QUERY_KEYS):self._reject_graphics(payload)
        controls = {}
        for item in fields:
            match = re.fullmatch(r'([a-z])=([A-Za-z0-9]{1,10})', item)
            if not match:self._reject_graphics(payload)
            key, value = match.groups()
            if key not in self.QUERY_KEYS or key in controls:self._reject_graphics(payload)
            controls[key] = value
        if controls.get('a') != 'q' or 'i' not in controls:self._reject_graphics(payload)
        numeric = {}
        for key in self.NUMERIC_KEYS & controls.keys():
            if not re.fullmatch(r'\d{1,10}', controls[key]):self._reject_graphics(payload)
            numeric[key] = int(controls[key])
        if not 1 <= numeric['i'] <= 4294967295:self._reject_graphics(payload)
        if any(key in numeric and not 1 <= numeric[key] <= 16384 for key in ('s', 'v')):
            self._reject_graphics(payload)
        if 'f' in numeric and numeric['f'] not in (24, 32, 100):self._reject_graphics(payload)
        if controls.get('t', 'd') not in ('d', 'f', 't', 's'):self._reject_graphics(payload)
        if numeric.get('q', 0) not in (0, 1, 2):self._reject_graphics(payload)
        # No read/decode/store/display occurs even for a file/shared-memory query.
        # Kitty permits a printable error response to a query. This response is
        # deliberately negative; it never reports OK or implemented rendering.
        # q=2 suppresses all replies; q=1 suppresses successful replies only.
        if numeric.get('q', 0) != 2:
            self._reply(f'\x1b_Gi={numeric["i"]};ENOTSUP:graphics unsupported\x1b\\'.encode())
