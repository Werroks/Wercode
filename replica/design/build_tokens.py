#!/usr/bin/env python3
"""Single source of truth for Wercode colour schemes.

Writes tokens.json (for contrast.py) and themes/<id>.toml (loaded by the app).

    python build_tokens.py
"""
import json
import pathlib

HERE = pathlib.Path(__file__).parent

# ui roles, 16 ANSI colours, syntax roles. All palettes are our own.
THEMES = {
    "black": {
        "name": "Black", "appearance": "dark",
        "ui": {
            "bg": "#000000", "surface": "#0c0c0c", "surface-2": "#161616",
            "border": "#262626", "border-input": "#6b6b6b",
            "text": "#e8e8e8", "text-muted": "#9a9a9a",
            "accent": "#5aa2ff", "on-accent": "#000000",
            "danger": "#ff6b6b", "success": "#5fd38d", "warning": "#e6c15a",
            "selection": "#1f3a5f",
        },
        "ansi": ["#1a1a1a", "#ff6b6b", "#5fd38d", "#e6c15a", "#5aa2ff", "#c58af9", "#4fd1c5", "#cfcfcf",
                 "#6e6e6e", "#ff8f8f", "#86e3a9", "#f2d583", "#86bbff", "#d8b0ff", "#7fe3d9", "#ffffff"],
        "syntax": {
            "keyword": "#c58af9", "string": "#5fd38d", "comment": "#7c7c7c", "function": "#5aa2ff",
            "type": "#4fd1c5", "number": "#e6c15a", "constant": "#ff9e64", "variable": "#e8e8e8",
            "operator": "#9a9a9a",
        },
    },
    "graphite": {
        "name": "Graphite", "appearance": "dark",
        "ui": {
            "bg": "#1c1e21", "surface": "#24272b", "surface-2": "#2d3136",
            "border": "#383d43", "border-input": "#808a95",
            "text": "#dfe3e8", "text-muted": "#a3acb6",
            "accent": "#79b4f5", "on-accent": "#101214",
            "danger": "#f47f7a", "success": "#72d695", "warning": "#dfb650",
            "selection": "#2f4a6b",
        },
        "ansi": ["#2d3136", "#f47f7a", "#72d695", "#dfb650", "#79b4f5", "#c9a4f2", "#5fcfd6", "#cdd3da",
                 "#808a95", "#f9a19c", "#93e3ae", "#ecca78", "#a0cbfa", "#dcc2f8", "#8adfe5", "#f5f7fa"],
        "syntax": {
            "keyword": "#c9a4f2", "string": "#72d695", "comment": "#8b949e", "function": "#79b4f5",
            "type": "#5fcfd6", "number": "#dfb650", "constant": "#f0a070", "variable": "#dfe3e8",
            "operator": "#a3acb6",
        },
    },
    "midnight": {
        "name": "Midnight", "appearance": "dark",
        "ui": {
            "bg": "#0b1020", "surface": "#121830", "surface-2": "#1a2242",
            "border": "#26305a", "border-input": "#6f7bb0",
            "text": "#dde3f7", "text-muted": "#9aa4cc",
            "accent": "#8fa8ff", "on-accent": "#0b1020",
            "danger": "#ff7a90", "success": "#63d6a4", "warning": "#e8c26a",
            "selection": "#2a3a78",
        },
        "ansi": ["#1a2242", "#ff7a90", "#63d6a4", "#e8c26a", "#8fa8ff", "#c39cf5", "#5cc9e8", "#c8cfe8",
                 "#6f7bb0", "#ff9dad", "#88e2bb", "#f1d48f", "#afc1ff", "#d7bcfa", "#8adaf0", "#f4f6ff"],
        "syntax": {
            "keyword": "#c39cf5", "string": "#63d6a4", "comment": "#7f89b5", "function": "#8fa8ff",
            "type": "#5cc9e8", "number": "#e8c26a", "constant": "#ff9f7a", "variable": "#dde3f7",
            "operator": "#9aa4cc",
        },
    },
    "forest": {
        "name": "Forest", "appearance": "dark",
        "ui": {
            "bg": "#0d1410", "surface": "#131d17", "surface-2": "#1b2920",
            "border": "#26382c", "border-input": "#6f8f7a",
            "text": "#dce8df", "text-muted": "#98af9f",
            "accent": "#7fd49a", "on-accent": "#0d1410",
            "danger": "#f08a7a", "success": "#a5dc7c", "warning": "#e2c36b",
            "selection": "#24452f",
        },
        "ansi": ["#1b2920", "#f08a7a", "#a5dc7c", "#e2c36b", "#7ab8e8", "#d3a0e0", "#6fd3c4", "#c9d6cc",
                 "#6f8f7a", "#f6a898", "#bde79c", "#edd591", "#9fcdf0", "#e1bcea", "#94e0d4", "#f3f8f4"],
        "syntax": {
            "keyword": "#d3a0e0", "string": "#a5dc7c", "comment": "#7d9585", "function": "#7ab8e8",
            "type": "#6fd3c4", "number": "#e2c36b", "constant": "#f0a070", "variable": "#dce8df",
            "operator": "#98af9f",
        },
    },
    "paper": {
        "name": "Paper", "appearance": "light",
        "ui": {
            "bg": "#fbfaf7", "surface": "#f1efe9", "surface-2": "#e6e3da",
            "border": "#d8d4c8", "border-input": "#8a8577",
            "text": "#1f1d1a", "text-muted": "#575349",
            "accent": "#2b5fc7", "on-accent": "#ffffff",
            "danger": "#b3261e", "success": "#1e7a45", "warning": "#8a5a00",
            "selection": "#cfe0ff",
        },
        "ansi": ["#1f1d1a", "#b3261e", "#1e7a45", "#8a5a00", "#2b5fc7", "#8a3fb0", "#0f7480", "#575349",
                 "#6f6a60", "#c2332a", "#1c7c44", "#8f5f00", "#2f63cc", "#9447ba", "#0e7682", "#3a372f"],
        "syntax": {
            "keyword": "#8a3fb0", "string": "#1e7a45", "comment": "#6f6a60", "function": "#2b5fc7",
            "type": "#0f7480", "number": "#8a5a00", "constant": "#a8471a", "variable": "#1f1d1a",
            "operator": "#575349",
        },
    },
}

ANSI_NAMES = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
              "bright-black", "bright-red", "bright-green", "bright-yellow",
              "bright-blue", "bright-magenta", "bright-cyan", "bright-white"]


def pairs_for(tid, theme):
    p = []
    for bg in ("bg", "surface", "surface-2"):
        p.append([f"{tid}-ui-text", f"{tid}-ui-{bg}"])
        p.append([f"{tid}-ui-text-muted", f"{tid}-ui-{bg}"])
    p.append([f"{tid}-ui-text", f"{tid}-ui-selection"])
    p.append([f"{tid}-ui-on-accent", f"{tid}-ui-accent"])
    for role in ("accent", "danger", "success", "warning"):
        p.append([f"{tid}-ui-{role}", f"{tid}-ui-bg"])
        p.append([f"{tid}-ui-{role}", f"{tid}-ui-surface"])
    p.append([f"{tid}-ui-border-input", f"{tid}-ui-bg", "ui"])
    p.append([f"{tid}-ui-accent", f"{tid}-ui-bg", "ui"])  # focus ring
    for i, n in enumerate(ANSI_NAMES):
        if i == 0:
            continue  # ANSI black is a background colour by convention
        if i == 8:
            p.append([f"{tid}-ansi-{n}", f"{tid}-ui-bg", "ui"])  # dim text, 3:1 floor
        else:
            p.append([f"{tid}-ansi-{n}", f"{tid}-ui-bg"])
    for role in theme["syntax"]:
        p.append([f"{tid}-syntax-{role}", f"{tid}-ui-bg"])
    return p


def toml_theme(theme):
    out = [f'name = "{theme["name"]}"', f'appearance = "{theme["appearance"]}"', "", "[ui]"]
    out += [f'{k.replace("-", "_")} = "{v}"' for k, v in theme["ui"].items()]
    out += ["", "[terminal]",
            f'foreground = "{theme["ui"]["text"]}"',
            f'background = "{theme["ui"]["bg"]}"',
            f'cursor = "{theme["ui"]["text"]}"',
            f'selection = "{theme["ui"]["selection"]}"',
            "ansi = [" + ", ".join(f'"{c}"' for c in theme["ansi"]) + "]",
            "", "[syntax]"]
    out += [f'{k} = "{v}"' for k, v in theme["syntax"].items()]
    return "\n".join(out) + "\n"


def main():
    color, pairs = {}, []
    for tid, t in THEMES.items():
        color[tid] = {"ui": t["ui"], "ansi": dict(zip(ANSI_NAMES, t["ansi"])), "syntax": t["syntax"]}
        pairs += pairs_for(tid, t)
    tokens = {
        "_about": "Generated by build_tokens.py. Do not edit by hand.",
        "color": color,
        "pairs": pairs,
        "font": {
            "ui": "system UI font (Segoe UI / SF / system sans)",
            "mono": "Cascadia Mono, Consolas, Menlo, DejaVu Sans Mono, monospace",
        },
        "type": {
            "xs": {"size": 11, "line": 16, "weight": 500},
            "sm": {"size": 12, "line": 18, "weight": 400},
            "base": {"size": 13, "line": 20, "weight": 400},
            "lg": {"size": 16, "line": 24, "weight": 600},
            "mono": {"size": 14, "line": 1.2, "weight": 400},
        },
        "space": [0, 2, 4, 6, 8, 12, 16, 24, 32],
        "radius": {"sm": 4, "md": 6, "lg": 8},
        "layout": {
            "titlebar": 36, "tabbar": 34, "panel-header": 36, "tree-row": 24, "icon": 16,
            "icon-button": 28, "panel-width": 300, "panel-min": 200, "panel-max-fraction": 0.5,
            "splitter-visible": 1, "splitter-hit": 6, "terminal-padding": 8,
            "window-min": [640, 400],
        },
        "motion": {"fast": "100ms", "base": "160ms"},
    }
    (HERE / "tokens.json").write_text(json.dumps(tokens, indent=2) + "\n", encoding="utf-8")
    tdir = HERE / "themes"
    tdir.mkdir(exist_ok=True)
    for tid, t in THEMES.items():
        (tdir / f"{tid}.toml").write_text(toml_theme(t), encoding="utf-8")
    print(f"wrote tokens.json ({len(pairs)} pairs) and {len(THEMES)} themes")


if __name__ == "__main__":
    main()
