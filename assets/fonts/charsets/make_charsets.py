"""Generate unicode text files listing characters for font subsetting.

Outputs (UTF-8, one line, no separators) in this directory:
  base.txt         ASCII + Latin-1 + CJK punct + fullwidth + CAD/math symbols
  tgscc3500.txt    通用规范汉字表 level 1 (3500)
  gb2312_l1.txt    GB2312 level-1 hanzi (3755)
  gb2312_all.txt   GB2312 all hanzi (6763) + GB2312 symbol rows 1-9
  set_ui.txt       base + union(tgscc3500, gb2312_l1)
  set_gb.txt       base + gb2312_all + tgscc3500 (so 3500 set is a subset)
"""
import os
here = os.path.dirname(os.path.abspath(__file__))

def rng(a, b):
    return [chr(c) for c in range(a, b + 1)]

base = []
base += rng(0x20, 0x7E)                 # ASCII
base += rng(0xA0, 0xFF)                 # Latin-1 (° ± ² ³ µ Ø × ÷ ...)
base += rng(0x2010, 0x2027)             # dashes, quotes, bullet, ellipsis
base += rng(0x2030, 0x203B)             # permille, primes, reference mark
base += list("€™℃℉№Ω℧ℓ⅓⅔¼½¾")
base += rng(0x2150, 0x215F)             # vulgar fractions
base += rng(0x2160, 0x216B)             # Roman numerals Ⅰ..Ⅻ
base += rng(0x2190, 0x2199)             # arrows
base += list("∀∂∃∅∆∇∈∉∑−∓∕∗∘∙√∝∞∟∠∡∥∦∧∨∩∪∫∮∴∵∼≈≌≠≡≤≥≦≧⊥⊿⌀⌒")
base += rng(0x0391, 0x03A9) + rng(0x03B1, 0x03C9)   # Greek (Δ φ Φ π ...)
base += rng(0x2460, 0x2473)             # circled numbers ①..⑳
base += rng(0x2500, 0x254B)             # box drawing (tables in UI)
base += list("■□▲△▼▽◆◇○◎●★☆")
base += rng(0x3000, 0x303F)             # CJK symbols & punctuation
base += rng(0x3200, 0x3229) if False else []
base += rng(0xFF01, 0xFF5E)             # fullwidth ASCII variants
base += rng(0xFFE0, 0xFFE6)             # fullwidth ￠￡￢￣￤￥￦
base += ["�"]                        # replacement char

tg = open(os.path.join(here, "npm/togscc/package/data/characters.txt"), encoding="utf-8").read().split()
assert len(tg) == 8105, len(tg)
tg3500 = tg[:3500]
tg6500 = tg[:6500]

def gb2312(rows):
    out = []
    for r in rows:
        for c in range(0xA1, 0xFF):
            try:
                ch = bytes([r, c]).decode("gb2312")
                out.append(ch)
            except UnicodeDecodeError:
                pass
    return out

gb_l1 = gb2312(range(0xB0, 0xD8))
gb_l2 = gb2312(range(0xD8, 0xF8))
gb_sym = gb2312(range(0xA1, 0xAA))
print("gb2312 L1", len(gb_l1), "L2", len(gb_l2), "sym", len(gb_sym))

def uniq(seq):
    seen = set(); out = []
    for ch in seq:
        if ch not in seen:
            seen.add(ch); out.append(ch)
    return out

def write(name, chars):
    chars = uniq(chars)
    with open(os.path.join(here, name), "w", encoding="utf-8", newline="") as f:
        f.write("".join(chars))
    print(f"{name}: {len(chars)} chars")
    return chars

write("base.txt", base)
write("tgscc3500.txt", tg3500)
write("gb2312_l1.txt", gb_l1)
write("gb2312_all.txt", gb_l1 + gb_l2 + gb_sym)
ui = write("set_ui.txt", base + tg3500 + gb_l1)
gb = write("set_gb.txt", base + gb_sym + gb_l1 + gb_l2 + tg3500)
write("set_tg6500.txt", base + tg6500 + gb_l1 + gb_l2 + gb_sym)
s3500 = set(tg3500); sl1 = set(gb_l1); sall = set(gb_l1 + gb_l2)
print("3500 & gbL1", len(s3500 & sl1), "3500-gbL1", len(s3500 - sl1), "gbL1-3500", len(sl1 - s3500))
print("3500 - gb_all", len(s3500 - sall), "".join(sorted(s3500 - sall))[:80])
print("tg6500 - gb_all", len(set(tg6500) - sall))
