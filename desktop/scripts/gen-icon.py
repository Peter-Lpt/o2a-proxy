"""Generate the o2a-proxy app icon set (signal-light + data-flow style).

Usage: python scripts/gen-icon.py [out_dir]
Overwrites the Tauri bundle icons under src-tauri/icons.
"""

import os
import sys

from PIL import Image, ImageDraw
import numpy as np


ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
OUT = os.path.join(ROOT, "desktop", "src-tauri", "icons")
if len(sys.argv) > 1:
    OUT = os.path.abspath(sys.argv[1])


def base_icon(size: int = 512) -> Image.Image:
    """蓝->绿 45° 渐变圆角方块 + 白色信号柱。"""
    s = size
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))

    # 45° 对角渐变
    c1 = np.array([79, 140, 255], dtype=np.float64)    # #4f8cff
    c2 = np.array([52, 211, 153], dtype=np.float64)    # #34d399
    y, x = np.mgrid[0:s, 0:s]
    t = (x + y) / (2.0 * (s - 1))
    rgb = (c1[None, None, :] * (1 - t[..., None]) + c2[None, None, :] * t[..., None])
    rgba = np.dstack([rgb, np.full((s, s), 255.0)])
    grad = Image.fromarray(rgba.astype(np.uint8), "RGBA")

    # 圆角遮罩
    mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (0, 0, s - 1, s - 1), radius=int(s * 0.22), fill=255
    )
    img.paste(grad, (0, 0), mask)

    d = ImageDraw.Draw(img)
    # 顶部内高光
    inset = max(1, int(s * 0.025))
    d.rounded_rectangle(
        (inset, inset, s - 1 - inset, s - 1 - inset),
        radius=int(s * 0.20),
        outline=(255, 255, 255, 36),
        width=max(2, int(s * 0.012)),
    )

    # 白色信号柱（数据流）
    bar_w = int(s * 0.115)
    gap = int(s * 0.062)
    base_y = int(s * 0.775)
    heights = [int(s * 0.30), int(s * 0.48), int(s * 0.66)]
    total_w = 3 * bar_w + 2 * gap
    start_x = (s - total_w) // 2
    for i, h in enumerate(heights):
        x0 = start_x + i * (bar_w + gap)
        y0 = base_y - h
        d.rounded_rectangle(
            (x0, y0, x0 + bar_w - 1, base_y),
            radius=bar_w // 2,
            fill=(255, 255, 255, 255),
        )
    return img


def save_scaled(path: str, size: int) -> None:
    base_icon(512).resize((size, size), Image.LANCZOS).save(path)


def main() -> None:
    os.makedirs(OUT, exist_ok=True)
    save_scaled(os.path.join(OUT, "icon.png"), 512)
    save_scaled(os.path.join(OUT, "32x32.png"), 32)
    save_scaled(os.path.join(OUT, "128x128.png"), 128)
    save_scaled(os.path.join(OUT, "128x128@2x.png"), 256)

    logos = {
        "Square30x30Logo.png": 30,
        "Square44x44Logo.png": 44,
        "Square71x71Logo.png": 71,
        "Square89x89Logo.png": 89,
        "Square107x107Logo.png": 107,
        "Square142x142Logo.png": 142,
        "Square150x150Logo.png": 150,
        "Square284x284Logo.png": 284,
        "Square310x310Logo.png": 310,
        "StoreLogo.png": 50,
    }
    for name, size in logos.items():
        save_scaled(os.path.join(OUT, name), size)

    # 多尺寸 ICO（含 256 的 PNG 压缩项）
    ico = base_icon(256)
    ico.save(
        os.path.join(OUT, "icon.ico"),
        format="ICO",
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )
    print(f"icons regenerated in {OUT}")


if __name__ == "__main__":
    main()
