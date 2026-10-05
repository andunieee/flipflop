#!/usr/bin/env python3
"""Turn the 24px line icons in ui/icons into Android vector drawables (stroke only; tinted by the UI),
which Compose Multiplatform resources load on every target.
Usage: tools/icons.py   (run from kmp/)"""
import re, subprocess, pathlib, xml.etree.ElementTree as ET

src = pathlib.Path("icons")
out = pathlib.Path("shared/src/commonMain/composeResources/drawable")
out.mkdir(parents=True, exist_ok=True)
NS = "{http://www.w3.org/2000/svg}"
f = lambda v: ("%g" % float(v))

def circle(cx, cy, r):
    cx, cy, r = float(cx), float(cy), float(r)
    return f"M{f(cx - r)},{f(cy)}a{f(r)},{f(r)} 0 1,0 {f(2 * r)},0a{f(r)},{f(r)} 0 1,0 {f(-2 * r)},0z"

def rect(x, y, w, h, rx):
    x, y, w, h, rx = map(float, (x, y, w, h, rx))
    r = rx
    return (f"M{f(x + r)},{f(y)}h{f(w - 2 * r)}a{f(r)},{f(r)} 0 0,1 {f(r)},{f(r)}v{f(h - 2 * r)}"
            f"a{f(r)},{f(r)} 0 0,1 {f(-r)},{f(r)}h{f(-(w - 2 * r))}a{f(r)},{f(r)} 0 0,1 {f(-r)},{f(-r)}"
            f"v{f(-(h - 2 * r))}a{f(r)},{f(r)} 0 0,1 {f(r)},{f(-r)}z")

for svg in sorted(src.glob("*.svg")):
    name = svg.stem.replace("-", "_")
    root = ET.parse(svg).getroot()
    paths = []
    for el in root.iter():
        tag = el.tag.replace(NS, "")
        a = el.attrib
        if tag == "path":
            paths.append(a["d"])
        elif tag == "circle":
            paths.append(circle(a["cx"], a["cy"], a["r"]))
        elif tag == "rect":
            paths.append(rect(a["x"], a["y"], a["width"], a["height"], a.get("rx", 0)))
        elif tag in ("line", "polyline", "ellipse", "polygon"):
            raise SystemExit(f"{svg}: unsupported <{tag}>")
    body = "\n".join(
        f'    <path android:pathData="{d}" android:strokeColor="#FF000000" android:strokeWidth="2"'
        f' android:strokeLineCap="round" android:strokeLineJoin="round"/>' for d in paths)
    (out / f"ic_{name}.xml").write_text(
        '<vector xmlns:android="http://schemas.android.com/apk/res/android"\n'
        '    android:width="24dp" android:height="24dp" android:viewportWidth="24" android:viewportHeight="24">\n'
        f"{body}\n</vector>\n")
print("ok")
