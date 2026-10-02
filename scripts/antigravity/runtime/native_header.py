"""Narrow, bounded, in-memory VT observer; never exports screen/string bodies.

The isolated pyte dependency supplies modeled cell operations. Exact documented
presentation/input controls may be inert; unknown controls fail closed. This is
not a keyboard encoder, window manager, clipboard, graphics terminal or xterm.
"""
import codecs
import copy
import re
import pyte


class NativeHeader:
    PRESENTATION_MODES = {5, 12, 25}
    INPUT_MODES = {9, 1000, 1001, 1002, 1003, 1004, 1005, 1006,
                   1015, 1016, 2004}
    MODIFIER_RESOURCES = {0, 1, 2, 3, 4, 6, 7}
    SCREEN_FINALS = set('@ABCDEFGHJKLMPXadefgrsu`IZSTb')

    def __init__(self, width=150, height=40):
        if not 20 <= width <= 500 or not 10 <= height <= 200:
            raise ValueError('terminal-size-unsupported')
        self.width, self.height = width, height
        self.main = pyte.Screen(width, height)
        self.alternate = pyte.Screen(width, height)
        self.screen = self.main
        self.stream = pyte.Stream(self.screen)
        self.decoder = codecs.getincrementaldecoder('utf-8')('replace')
        self.pending = ''
        self.total = 0
        self.control_families = set()
        self.replies = []
        self.reply_count = 0
        self.unsupported_control = None
        self.header_positions = []
        self.synchronized = False
        self.failed = False
        self.modes = {25: True, 7: True, 6: False, 2026: False}
        self.modifier_preferences = {key: 0 for key in self.MODIFIER_RESOURCES}
        self.keyboard_preferences = {False: [0], True: [0]}
        self.colours = {10: 'ffff/ffff/ffff', 11: '0000/0000/0000',
                        12: 'ffff/ffff/ffff'}
        self.default_colours = dict(self.colours)

    @property
    def grid(self):
        return [[(self.screen.buffer[y][x].data or ' ')[0] for x in range(self.width)]
                for y in range(self.height)]

    def _reply(self, value):
        self.reply_count += 1
        if self.reply_count > 64 or sum(map(len, self.replies)) + len(value) > 4096:
            self.failed = True
            raise ValueError('terminal-reply-budget-exceeded')
        self.replies.append(value)

    def take_replies(self):
        result = b''.join(self.replies)
        self.replies.clear()
        return result

    def _reject(self, family, params='', intermediate='', final=''):
        # Numeric shape only: no printable string payload, title or chat data.
        self.failed = True
        prefix = params[:1] if params and params[:1] in '?><=' else None
        numeric = params[1:] if prefix else params
        fields = re.split('[;:]', numeric) if numeric else []
        valid = len(fields) <= 32 and all(re.fullmatch(r'\d{0,6}', x) for x in fields)
        self.unsupported_control = {
            'family': family, 'final': final[:1], 'intermediate': intermediate[:4],
            'privatePrefix': prefix, 'parameterLength': min(len(params), 4096),
            'numericParameters': [int(x) if x else None for x in fields] if valid else [],
            'numericShapeValid': bool(valid), 'parameterCount': min(len(fields), 33)}
        raise ValueError('unsupported-terminal-movement' if family == 'CSI' and
                         final in self.SCREEN_FINALS else 'terminal-control-unsupported')

    def _numbers(self, params, maximum=32):
        if not re.fullmatch(r'[0-9;]*', params):
            return None
        fields = params.split(';')
        if len(fields) > maximum or any(len(x) > 6 for x in fields):
            return None
        return [int(x) if x else 0 for x in fields]

    def _switch_screen(self, alternate):
        self.screen = self.alternate if alternate else self.main
        self.stream = pyte.Stream(self.screen)

    def _soft_reset(self):
        buffer, cursor = copy.deepcopy(self.screen.buffer), copy.copy(self.screen.cursor)
        self.screen.reset()
        self.screen.buffer = buffer
        self.screen.cursor.x, self.screen.cursor.y = cursor.x, cursor.y
        # DECSTR resets saved cursor/state while retaining visible cells/position.
        self.screen.save_cursor()
        self.screen.savepoints[-1].cursor.x = self.screen.savepoints[-1].cursor.y = 0
        self.modes = {25: True, 7: True, 6: False, 2026: False}
        self.synchronized = False

    def _keyboard(self, params):
        prefix, values = params[:1], self._numbers(params[1:], 2)
        stack = self.keyboard_preferences[self.screen is self.alternate]
        if prefix == '?' and params == '?':
            # Zero is truthful only when requested flags are zero. Nonzero
            # preferences do not install an encoder, so advertise no support.
            if stack[-1] == 0:self._reply(b'\x1b[?0u')
            return True
        if values is None:return False
        if prefix == '>' and len(values) == 1 and 0 <= values[0] <= 31:
            stack.append(values[0])
            if len(stack) > 16:del stack[0]
            return True
        if prefix == '<' and len(values) == 1 and 0 <= values[0] <= 16:
            count = values[0] or 1
            del stack[max(0, len(stack) - count):]
            if not stack:stack.append(0)
            return True
        if prefix == '=' and len(values) <= 2 and 0 <= values[0] <= 31:
            mode = (values[1] if len(values) == 2 else 1) or 1
            if mode not in (1, 2, 3):return False
            stack[-1] = values[0] if mode == 1 else (
                stack[-1] | values[0] if mode == 2 else stack[-1] & ~values[0])
            return True
        return False

    def _csi(self, sequence, params, intermediate, final):
        self.control_families.add('CSI')
        if len(params) > 256 or len(intermediate) > 2:
            self._reject('CSI', params, intermediate, final)
        numbers = self._numbers(params)
        if final == 'c' and not intermediate and params in ('', '0', '>', '>0'):
            self._reply(b'\x1b[>0;0;0c' if params.startswith('>') else b'\x1b[?6c')
        elif final == 'n' and not intermediate and params in ('5', '6', '?6'):
            if params == '5':self._reply(b'\x1b[0n')
            else:
                row = self.screen.cursor.y + 1
                # CPR in origin mode is relative to the top margin.
                if self.modes.get(6) and self.screen.margins:row -= self.screen.margins.top
                prefix = '?' if params.startswith('?') else ''
                self._reply(f'\x1b[{prefix}{row};{self.screen.cursor.x + 1}R'.encode())
        elif final == 'n' and not intermediate and params.startswith('>'):
            values = self._numbers(params[1:], 1)
            key = 2 if params == '>' else (values[0] if values else -1)
            if key not in self.MODIFIER_RESOURCES:self._reject('CSI', params, intermediate, final)
            self.modifier_preferences[key] = -1
        elif final == 'p' and intermediate == '$' and re.fullmatch(r'\??\d{1,4}', params):
            key = int(params.lstrip('?'))
            value = (1 if self.modes[key] else 2) if key in self.modes else 0
            # Standard 4/20 are pyte-modeled; unknown is honestly unsupported.
            if not params.startswith('?'):
                value = (1 if key in self.screen.mode else 2) if key in (4, 20) else 0
            self._reply(f'\x1b[{params};{value}$y'.encode())
        elif final == 'p' and intermediate == '!' and not params:self._soft_reset()
        elif final == 'q' and intermediate == ' ' and numbers is not None and len(numbers) == 1 and numbers[0] <= 7:
            pass  # Cursor shape only; no position/text effect.
        elif final == 'q' and not intermediate and params in ('>', '>0'):
            self._reply(b'\x1bP>|AIC bounded terminal 0\x1b\\')
        elif final in ('p', 's') and not intermediate and re.fullmatch(r'>[0-3]?', params):
            if final == 's' and params not in ('>', '>0', '>1'):
                self._reject('CSI', params, intermediate, final)
            # Exact pointer-hide/shift-mouse input preferences. No mouse input
            # is generated by this observer and no cursor/cell is changed.
        elif final == 'u' and not intermediate and params and params[:1] in '?><=':
            if not self._keyboard(params):self._reject('CSI', params, intermediate, final)
        elif final == 'u' and not params and not intermediate:self.screen.restore_cursor()
        elif final == 's' and not params and not intermediate:self.screen.save_cursor()
        elif final == 'W' and params == '?5' and not intermediate:
            self.screen.tabstops = set(range(8, self.width, 8))
        elif final == 't' and not intermediate and params in ('18', '19'):
            # Logical screen equals this observer's configured geometry.
            kind = 8 if params == '18' else 9
            self._reply(f'\x1b[{kind};{self.height};{self.width}t'.encode())
        elif final == 'm' and not intermediate and params.startswith('>'):
            match = re.fullmatch(r'>(?:([0123467])(?:;([012]))?)?', params)
            mask = re.fullmatch(r'>4:([0-7])', params)
            if mask:pass  # Mask preference only; no generated keyboard input.
            elif match:
                key, value = match.groups()
                if key is None:self.modifier_preferences.update({k: 0 for k in (1, 2, 3, 4)})
                else:self.modifier_preferences[int(key)] = int(value or '0')
            else:self._reject('CSI', params, intermediate, final)
        elif final == 'm' and not intermediate and re.fullmatch(r'\?[0123467]', params):
            key = int(params[1:])
            if self.modifier_preferences[key] in (-1, 0):
                self._reply(f'\x1b[>{key};{self.modifier_preferences[key]}m'.encode())
        elif final == 'm' and not intermediate and re.fullmatch(r'[0-9;:]*', params) and len(params) <= 128:
            # Standard rendition affects styling only. Bounded colon syntax is
            # validated separately from private keyboard resource operations.
            fields = re.split('[;:]', params)
            if len(fields) > 32 or any(len(x) > 3 for x in fields):self._reject('CSI', params, intermediate, final)
            if ':' not in params:self.stream.feed(sequence)
        elif final in ('h', 'l') and not intermediate:
            private = params.startswith('?')
            values = self._numbers(params[1:] if private else params)
            allowed = ({6, 7, 47, 1047, 1048, 1049, 2026} | self.PRESENTATION_MODES |
                       self.INPUT_MODES) if private else {4, 20}
            if values is None or not values or any(key not in allowed for key in values):
                self._reject('CSI', params, intermediate, final)
            enabled = final == 'h'
            for key in values:
                if not private:self.stream.feed(f'\x1b[{key}{final}')
                elif key in (47, 1047, 1049):
                    if key == 1049 and enabled:self.main.save_cursor(); self.alternate.reset()
                    self._switch_screen(enabled)
                    if key == 1047 and not enabled:self.alternate.reset()
                    if key == 1049 and not enabled:self.main.restore_cursor()
                elif key == 1048:
                    self.screen.save_cursor() if enabled else self.screen.restore_cursor()
                elif key in (6, 7, 5, 25):self.stream.feed(f'\x1b[?{key}{final}')
                elif key == 2026:self.synchronized = enabled
                self.modes[key] = enabled
        elif final in pyte.Stream.csi and final not in 'chlmnpqrst' and not intermediate and numbers is not None:
            expected = 2 if final in 'Hfr' else 1
            if len(numbers) > expected or (final in 'JK' and numbers[0] not in (0, 1, 2)) or (final == 'g' and numbers[0] not in (0, 3)):
                self._reject('CSI', params, intermediate, final)
            self.stream.feed(sequence)
        elif final == '`' and not intermediate and numbers is not None and len(numbers) == 1:
            self.screen.cursor_to_column(numbers[0] or 1)
        elif final == 'r' and not intermediate and numbers is not None and len(numbers) <= 2:
            self.stream.feed(sequence)
        else:self._reject('CSI', params, intermediate, final)

    def _string(self, family, payload, terminator):
        name = {'P': 'DCS', ']': 'OSC', '_': 'APC', '^': 'PM'}[family]
        self.control_families.add(name)
        if family == ']':
            selector, _, body = payload.partition(';')
            if selector in ('10', '11', '12'):
                key = int(selector)
                if body == '?':self._reply(f'\x1b]{selector};rgb:{self.colours[key]}{terminator}'.encode())
                elif re.fullmatch(r'rgb:[0-9a-fA-F]{4}/[0-9a-fA-F]{4}/[0-9a-fA-F]{4}', body):
                    self.colours[key] = body[4:].lower()
                else:self._reject(name)
            elif selector in ('110', '111', '112') and not body:
                key = int(selector) - 100; self.colours[key] = self.default_colours[key]
            elif selector in ('0', '1', '2', '7', '8'):
                pass  # Metadata only; no OS/title/cwd/identity effect or fetch.
            elif selector == '133' and re.fullmatch(r'[ABCD](?:;\d{1,6})?', body):pass
            elif selector == '52':pass  # Explicit blocked clipboard, no response.
            else:self._reject(name)
        elif family == 'P' and payload.startswith('$q'):
            self._reply(b'\x1bP0$r\x1b\\')  # All setting selectors unsupported.
        elif family == 'P' and re.fullmatch(r'\+q(?:[0-9A-Fa-f]{2}){1,64}', payload):
            self._reply(b'\x1bP0+r\x1b\\')  # Negative must not echo the name.
        elif family == 'P' and payload.startswith('>|'):pass  # Bounded version metadata only.
        else:self._reject(name)

    def feed(self, raw):
        self.total += len(raw)
        if self.total > 2 * 1024 * 1024:
            self.failed = True
            raise ValueError('native-output-budget-exceeded')
        self.pending += self.decoder.decode(raw)
        offset = 0
        while offset < len(self.pending):
            if self.pending[offset] != '\x1b':
                stop = self.pending.find('\x1b', offset)
                stop = len(self.pending) if stop < 0 else stop
                text = self.pending[offset:stop]
                if any(0x80 <= ord(x) <= 0x9f for x in text):self._reject('C1')
                self.stream.feed(text.replace('\x05', ''))  # Empty ENQ answerback.
                offset = stop; continue
            if offset + 1 >= len(self.pending):break
            family = self.pending[offset + 1]
            if family == '[':
                tail = self.pending[offset:]
                cancel = re.search('[\x18\x1a]', tail[2:])
                match = re.match(r'\x1b\[([0-?]*)([ -/]*)([@-~])', tail)
                if cancel and (not match or cancel.start() + 2 < len(match.group(0))):
                    offset += cancel.start() + 3; continue
                if not match:
                    if re.search('[^0-? -/]', tail[2:]):self._reject('CSI')
                    break
                self._csi(match.group(0), *match.groups())
                offset += len(match.group(0)); continue
            if family in ']P_^':
                tail = self.pending[offset + 2:]
                # OSC alone accepts BEL. DCS/APC/PM require ST.
                end = re.search(r'\x07|\x1b\\' if family == ']' else r'\x1b\\', tail)
                cancel = re.search('[\x18\x1a]', tail)
                if cancel and (not end or cancel.start() < end.start()):
                    offset += 3 + cancel.start(); continue
                if not end:break
                if end.start() > 4096:self._reject({'P':'DCS', ']':'OSC', '_':'APC', '^':'PM'}[family])
                self._string(family, tail[:end.start()], end.group())
                offset += 2 + end.end(); continue
            count = 3 if family in '()#%' else 2
            if offset + count > len(self.pending):break
            sequence = self.pending[offset:offset + count]
            if sequence not in ('\x1b7', '\x1b8', '\x1bD', '\x1bE', '\x1bM', '\x1bH', '\x1bc',
                                '\x1b(B', '\x1b(0', '\x1b)B', '\x1b)0', '\x1b#8'):
                self._reject('ESC', final=family)
            self.stream.feed(sequence)
            offset += count
        self.pending = self.pending[offset:]
        if len(self.pending) > 4096:
            self.failed = True
            raise ValueError('unsupported-terminal-sequence')

    def email(self):
        if self.failed or self.synchronized or self.pending:return None
        matches = []
        rows = [''.join(row) for row in self.grid]
        for index, row in enumerate(rows[:5]):
            title = re.search(r'\bAntigravity CLI v?\d+\.\d+\.\d+(?:\S*)?\s*$', row)
            if title is None:continue
            account = rows[index + 1][title.start():].strip()
            if re.fullmatch(r'[A-Za-z0-9.!#$%&\'*+/=?^_`{|}~-]+@'
                            r'[A-Za-z0-9.-]+\.[A-Za-z]{2,}', account):
                matches.append((account, index + 2, title.start() + 1))
        self.header_positions = [(row, column) for _, row, column in matches]
        return matches[0][0] if len(matches) == 1 else None
