"""Incremental VT screen decoder for the PTY and ConPTY acceptance harness.

Only printable cells survive: cursor addressing and erasure update the screen,
and styling, OSC titles, mouse modes and cursor visibility never become text.
"""
import codecs
import re
import unicodedata


def verify_fragmentation(capture):
    """Replay actual platform output with splits inside UTF-8 and VT sequences."""
    whole = Screen()
    whole.feed(capture)
    for size in (1, 7, 127):
        fragmented = Screen()
        for offset in range(0, len(capture), size):
            fragmented.feed(capture[offset:offset + size])
        assert fragmented.text == whole.text, f"VT replay differs with {size}-byte fragments"
        assert fragmented.pending == whole.pending


class Screen:
    def __init__(self, width=160, height=40):
        self.width, self.height = width, height
        self.cells = [[" "] * width for _ in range(height)]
        self.x = self.y = 0
        self.saved = (0, 0)
        self.primary = None
        self.pending = ""
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.queries = 0

    @property
    def text(self):
        return "\n".join("".join(row).rstrip() for row in self.cells)

    def feed(self, data):
        self.pending += self.decoder.decode(data)
        while self.pending:
            if self.pending[0] == "\x1b":
                if len(self.pending) < 2:
                    break
                if self.pending[1] == "[":
                    match = re.match(r"\x1b\[([0-?]*)([ -/]*)([@-~])", self.pending)
                    if not match:
                        break
                    self.csi(match[1], match[3])
                    self.pending = self.pending[match.end():]
                elif self.pending[1] == "]":
                    match = re.search(r"\x07|\x1b\\", self.pending[2:])
                    if not match:
                        break
                    self.pending = self.pending[2 + match.end():]
                else:
                    if self.pending[1] == "7":
                        self.saved = (self.x, self.y)
                    elif self.pending[1] == "8":
                        self.x, self.y = self.saved
                    elif self.pending[1] in "()":
                        if len(self.pending) < 3:
                            break
                        self.pending = self.pending[1:]
                    self.pending = self.pending[2:]
                continue
            char, self.pending = self.pending[0], self.pending[1:]
            if char == "\r":
                self.x = 0
            elif char == "\n":
                self.y += 1
                if self.y >= self.height:
                    self.cells.pop(0)
                    self.cells.append([" "] * self.width)
                    self.y = self.height - 1
            elif char == "\b":
                self.x = max(0, self.x - 1)
            elif char == "\t":
                self.x = min(self.width - 1, (self.x // 8 + 1) * 8)
            elif char >= " " and char != "\x7f":
                if unicodedata.combining(char):
                    if self.x:
                        self.cells[self.y][self.x - 1] += char
                    continue
                if self.x >= self.width:
                    self.x = 0
                    self.y = min(self.height - 1, self.y + 1)
                self.cells[self.y][self.x] = char
                self.x += 2 if unicodedata.east_asian_width(char) in "WF" else 1

    def csi(self, raw, final):
        private = raw.startswith("?")
        values = [int(v or 0) for v in raw.lstrip("?=>").split(";")]
        n = values[0] or 1
        if final in "Hf":
            self.y = min(self.height - 1, n - 1)
            self.x = min(self.width - 1, (values[1] or 1) - 1 if len(values) > 1 else 0)
        elif final in "ABCD":
            dx, dy = {"A": (0, -n), "B": (0, n), "C": (n, 0), "D": (-n, 0)}[final]
            self.x = min(self.width - 1, max(0, self.x + dx))
            self.y = min(self.height - 1, max(0, self.y + dy))
        elif final == "G":
            self.x = min(self.width - 1, n - 1)
        elif final == "d":
            self.y = min(self.height - 1, n - 1)
        elif final == "J":
            mode = values[0]
            for y in range(self.height):
                for x in range(self.width):
                    if mode in (2, 3) or (mode == 0 and (y, x) >= (self.y, self.x)) or (mode == 1 and (y, x) <= (self.y, self.x)):
                        self.cells[y][x] = " "
        elif final in "KX":
            start, end = (self.x, min(self.width, self.x + n)) if final == "X" else {
                0: (self.x, self.width), 1: (0, self.x + 1), 2: (0, self.width)
            }[values[0]]
            self.cells[self.y][start:end] = [" "] * (end - start)
        elif final == "s":
            self.saved = (self.x, self.y)
        elif final == "u":
            self.x, self.y = self.saved
        elif final == "n" and values[0] == 6:
            self.queries += 1
        elif private and final in "hl" and any(v in (47, 1047, 1049) for v in values):
            if final == "h" and self.primary is None:
                self.primary = (self.cells, self.x, self.y)
                self.cells = [[" "] * self.width for _ in range(self.height)]
                self.x = self.y = 0
            elif final == "l" and self.primary is not None:
                self.cells, self.x, self.y = self.primary
                self.primary = None
