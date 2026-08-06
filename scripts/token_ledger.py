#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
token_ledger.py —— Vtuber_Dream 代码体积 / token 台账生成器

用途
----
扫描 crates/*/src、crates/*/tests、web/src（排除 node_modules/dist/target 目录），
对每个 .rs / .ts / .tsx / .css 文件统计：
  - LOC      ：物理行数（含注释与空行）
  - bytes    ：文件字节数
  - token    ：估算 token 数（见「口径」）
并给出模块小计、总计，以及对超红线 Rust 源文件做「备书锚」审计。

用途定位：本项目里「代码 = AI 上下文 token 开销」治理的例行对账工具，
只用于相对账（本次提交涨/落了多少），不承诺与任何真实 tokenizer 精确一致。

口径（估算，非真值）
--------------------
  token = 非 ASCII 字符数 × 1           （每个非 ASCII 字符计 1 token）
        + ASCII 字符数 ÷ 4              （每 4 个 ASCII 字符计 1 token）

  理由：与 m1-code-eval 的实测（Rust 约 14 token/行，见
  docs/2026-08-03-m1-code-eval.md）同量级——Rust 平均每行约 40~60 ASCII 字符，
  除以 4 即得 ~10~15 token/行。该口径只用于相对度量，不做绝对值承诺。

红线审计（只针对 Rust src，test 目录豁免）
-----------------------------------------
  - 文件 > 500 行：要求文件「前 40 行」内出现字符串「体积备书」，否则违规。
  - 文件 > 800 行：额外要求「前 40 行」内出现字符串「拆分锚」，否则违规。

本项目 token 治理原则（引用自 AGENTS.md / 评审纪律）：
  「每行进 AI 上下文，>500 备书 / >800 拆分锚」——
  超过 500 行须在头部留下体积备书说明为何保留；超过 800 行须在头部留下拆分锚，
  说明何时 / 按何依据拆分。

tests-fixtures/ 目录单独只报总量（含 bytes/token），不计入红线与 LOC 红线。

退出码合约
----------
  --ci 模式：存在违规清单（红线缺备书/拆分锚，或某技术性错误）→ 退出码 1；
             否则退出码 0。
  非 --ci 模式：正常完成退出码 0；内部错误退出码 2。

命令行
------
  python scripts/token_ledger.py                打印汇总到 stdout
  python scripts/token_ledger.py --out <path>   额外（或改为）写完整 markdown 台账
  python scripts/token_ledger.py --ci           仅汇总裁决：有违规 exit 1，否则 exit 0
  python scripts/token_ledger.py --repo-root <dir>  指定仓库根（默认本脚本上级）

仅标准库，零依赖，单文件，Python 3.8+。
"""

from __future__ import annotations

import argparse
import os
import sys

# ---------------------------------------------------------------------------
# 常量
# ---------------------------------------------------------------------------

SRC_EXT = (".rs", ".ts", ".tsx", ".css")

RED_LINE_500 = 500
RED_LINE_800 = 800
ANCHOR_NOTE_500 = "体积备书"
ANCHOR_NOTE_800 = "拆分锚"
ANCHOR_SCAN_LINES = 40

EXCLUDE_DIRS = {"node_modules", "dist", "target"}

# web 分组：目录名 -> 标题
WEB_GROUPS = [
    ("src 根", ""),          # web/src/*.ts/.tsx/.css 直接件
    ("pages", "pages"),
    ("components", "components"),
    ("__tests__", "__tests__"),
    ("其余", None),          # hooks 等未归类目录，兜底
]


def is_text_source(p: str) -> bool:
    return p.endswith(SRC_EXT)


def iter_src_files(directory: str):
    """递归遍历 directory 下的源文件，跳过排除目录。"""
    for root, dirs, files in os.walk(directory):
        # 就地裁剪排除目录
        dirs[:] = [d for d in dirs if d not in EXCLUDE_DIRS]
        for f in sorted(files):
            p = os.path.join(root, f)
            if is_text_source(p):
                yield p


def file_metrics(path: str):
    """返回 (loc, nbytes, non_ascii, ascii) 。"""
    with open(path, "rb") as fh:
        raw = fh.read()
    nbytes = len(raw)
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        # 兜底：按 latin-1 解码以继续统计（主要影响 bytes 与 ascii 估算）
        text = raw.decode("latin-1", errors="replace")

    # 物理行数：按换行符切分，文件末尾未换行也算一行；与 wc -l 对齐
    if raw.endswith(b"\n"):
        loc = raw.count(b"\n")
    else:
        loc = raw.count(b"\n") + 1

    non_ascii = sum(1 for ch in text if ord(ch) > 127)
    ascii_chars = len(text) - non_ascii
    return loc, nbytes, non_ascii, ascii_chars


def est_tokens(non_ascii: int, ascii_chars: int) -> int:
    """token 估算：非 ASCII 1 token/字；ASCII 4 字符/token。"""
    from math import ceil
    return non_ascii + ceil(ascii_chars / 4)


# live-core 顶层模块（其余为 src 根直辖件）
CORE_TOP_MODULES = ("agent", "graph", "collector", "bilibili", "episodes")


def classify(path: str, repo_root: str):
    """归类 (组标题, 是否 rust-src, 是否豁免红线)。"""
    rel = os.path.relpath(path, repo_root)
    parts = rel.replace("\\", "/").split("/")

    # 默认兜底
    is_rust_src = False
    exempt_redline = False
    group = rel

    if len(parts) >= 2 and parts[0] == "crates":
        crate = parts[1]
        rest = parts[2:]
        if rest and rest[0] == "src":
            is_rust_src = path.endswith(".rs")
            exempt_redline = False
            modpath = rest[1:]
            if modpath and modpath[0] in CORE_TOP_MODULES:
                group = f"crates/{crate}/src/{modpath[0]}"
            elif crate == "live-server":
                group = "crates/live-server/src"
            else:
                group = f"crates/{crate}/src 根直辖件"
        elif rest and rest[0] == "tests":
            is_rust_src = path.endswith(".rs")
            exempt_redline = True
            group = f"crates/{crate}/tests"
    elif len(parts) >= 2 and parts[0] == "web" and parts[1] == "src":
        modpath = parts[2:]
        if len(modpath) <= 1:
            # 直接位于 web/src 根的文件（App.tsx / api.ts / styles.css 等）
            group = "web/src 根"
        else:
            top = modpath[0]
            if top == "pages" or top == "components" or top == "__tests__":
                group = f"web/{top}"
            else:
                # hooks 等未列入清单的目录
                group = "web/其他"

    return group, is_rust_src, exempt_redline


def audit(path: str, loc: int) -> dict:
    """红线审计。返回 {"类型": bool 违规} 或 None（无需审计）。"""
    result = {"500": None, "800": None}
    if loc > RED_LINE_800:
        result["800"] = True
        result["500"] = True
    elif loc > RED_LINE_500:
        result["500"] = True
        result["800"] = None
    else:
        return None

    # 扫描前 40 行是否含锚
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        head = []
        for i, line in enumerate(fh):
            if i >= ANCHOR_SCAN_LINES:
                break
            head.append(line)
    head_text = "".join(head)
    if loc > RED_LINE_800:
        result["500"] = ANCHOR_NOTE_500 not in head_text
        result["800"] = ANCHOR_NOTE_800 not in head_text
    elif loc > RED_LINE_500:
        result["500"] = ANCHOR_NOTE_500 not in head_text
    return result


def fmt(n) -> str:
    return "{:,}".format(int(n))


# ---------------------------------------------------------------------------
# 汇总
# ---------------------------------------------------------------------------

def collect(repo_root: str):
    """收集全部文件统计，返回 (rows, groups, violations, tests_fixtures_total)。

    rows: [{path, rel, group, loc, nbytes, token}]
    """
    rows = []
    violations = []  # (rel, loc, 违规项描述)

    # 各扫描根：crates/*/src、crates/*/tests、web/src
    scan_roots = [os.path.join(repo_root, "web", "src")]
    crates_dir = os.path.join(repo_root, "crates")
    if os.path.isdir(crates_dir):
        for crate in sorted(os.listdir(crates_dir)):
            crate_dir = os.path.join(crates_dir, crate)
            if not os.path.isdir(crate_dir):
                continue
            for sub in ("src", "tests"):
                sub_path = os.path.join(crate_dir, sub)
                if os.path.isdir(sub_path):
                    scan_roots.append(sub_path)

    for root in scan_roots:
        for p in iter_src_files(root):
            loc, nbytes, na, ac = file_metrics(p)
            token = est_tokens(na, ac)
            group, is_rust_src, exempt = classify(p, repo_root)
            rel = os.path.relpath(p, repo_root).replace("\\", "/")
            rows.append({
                "path": p,
                "rel": rel,
                "group": group,
                "loc": loc,
                "nbytes": nbytes,
                "token": token,
            })
            # 红线审计（仅 rust src，test 豁免）
            if is_rust_src and not exempt:
                a = audit(p, loc)
                if a:
                    for which, bad in a.items():
                        if bad:
                            need = ANCHOR_NOTE_800 if which == "800" else ANCHOR_NOTE_500
                            violations.append(
                                (rel, loc, f">{RED_LINE_800 if which == '800' else RED_LINE_500} 行缺「{need}」")
                            )

    # tests-fixtures 单独统计总量（只报总量，计入 bytes/token，不算红线）
    # 夹具多为 .json/.jsonl/.md/.gitignore 等文本文件，故统计该目录全部文件而非仅源扩展名。
    fixtures_total = {"loc": 0, "nbytes": 0, "token": 0}
    fixtures_dir = os.path.join(repo_root, "tests-fixtures")
    if os.path.isdir(fixtures_dir):
        for rootdir, dirs, files in os.walk(fixtures_dir):
            dirs[:] = [d for d in dirs if d not in EXCLUDE_DIRS]
            for f in sorted(files):
                p = os.path.join(rootdir, f)
                if os.path.basename(p) == ".gitkeep":
                    continue
                loc, nbytes, na, ac = file_metrics(p)
                fixtures_total["loc"] += loc
                fixtures_total["nbytes"] += nbytes
                fixtures_total["token"] += est_tokens(na, ac)

    return rows, violations, fixtures_total


def to_markdown(repo_root: str, rows, violations, fixtures_total, totals=None):
    """生成中文表格式台账 markdown。totals 为本体（非 fixtures）合计 dict。"""
    if totals is None:
        totals = _compute_totals(rows)
    L = []
    L.append("# 代码体积 token 台账")
    L.append("")
    L.append("> 生成：`python scripts/token_ledger.py`（估算口径，非真值）")
    L.append(">")
    L.append("> 口径：token = 非 ASCII 字符 ×1 + ASCII 字符 ÷4。只用于相对账，与")
    L.append("> m1-code-eval 实测 Rust ~14 token/行同量级。")
    L.append(">")
    L.append("> 治理原则：**每行进 AI 上下文，>500 备书 / >800 拆分锚**（AGENTS.md 引用）；")
    L.append("> 该红线仅约束 crates/*/src 的 Rust 文件，test 目录豁免。")
    L.append("")

    # 按 group 分组求小计
    order = _group_order(rows)
    L.append("## 汇总")
    L.append("")
    L.append("| 模块 | 文件 | LOC | bytes | 估 token |")
    L.append("|---|---:|---:|---:|---:|")
    grand = {"files": 0, "loc": 0, "nbytes": 0, "token": 0}
    for g in order:
        gr = [r for r in rows if r["group"] == g]
        f = len(gr)
        loc = sum(r["loc"] for r in gr)
        nb = sum(r["nbytes"] for r in gr)
        tk = sum(r["token"] for r in gr)
        L.append(f"| {g} | {f} | {fmt(loc)} | {fmt(nb)} | {fmt(tk)} |")
        grand["files"] += f
        grand["loc"] += loc
        grand["nbytes"] += nb
        grand["token"] += tk
    # fixtures 单独一行（不算 LOC 红线，仅报量）
    L.append(
        f"| tests-fixtures（仅报量） | {''} | {fmt(fixtures_total['loc'])} | "
        f"{fmt(fixtures_total['nbytes'])} | {fmt(fixtures_total['token'])} |"
    )
    L.append(f"| **总计（不含 fixtures）** | {grand['files']} | {fmt(grand['loc'])} | "
             f"{fmt(grand['nbytes'])} | {fmt(grand['token'])} |")
    L.append("")

    # 逐文件明细表
    L.append("## 逐文件明细")
    L.append("")
    L.append("| 模块 | 文件 | 行 | bytes | 估 token |")
    L.append("|---|---|---:|---:|---:|")
    for g in order:
        gr = [r for r in rows if r["group"] == g]
        for r in sorted(gr, key=lambda x: x["rel"]):
            L.append(f"| {g} | `{r['rel']}` | {fmt(r['loc'])} | {fmt(r['nbytes'])} | {fmt(r['token'])} |")
    L.append("")

    # 违规清单
    L.append("## 红线审计（>500 备书 / >800 拆分锚）")
    L.append("")
    if violations:
        L.append("| 文件 | 行 | 违规项 |")
        L.append("|---|---:|---|")
        for rel, loc, desc in sorted(violations, key=lambda x: -x[1]):
            L.append(f"| `{rel}` | {fmt(loc)} | {desc} |")
    else:
        L.append("无违规。所有超过 500 行（及 800 行）的 Rust 源文件，头部均含合规锚。")
    L.append("")
    return "\n".join(L)


def _compute_totals(rows):
    return {
        "files": len(rows),
        "loc": sum(r["loc"] for r in rows),
        "nbytes": sum(r["nbytes"] for r in rows),
        "token": sum(r["token"] for r in rows),
    }


def _group_order(rows):
    """返回 group 标题的展示顺序（保持可读）。web 分组固定次序，其余按首见顺序。"""
    web_order = ["web/src 根", "web/pages", "web/components", "web/__tests__", "web/其他"]
    order = []
    # 先 web 分组（在其首次出现处插入固定有序序列）
    web_groups_present = []
    for g in web_order:
        if any(r["group"] == g for r in rows):
            web_groups_present.append(g)
    for g in web_groups_present:
        order.append(g)
    # 其余 group 按类别首见（跳过已加入的 web 分组）
    for r in rows:
        g = r["group"]
        if g not in order and g != "web/其他" and not g.startswith("web/"):
            order.append(g)
    return order


def print_summary(repo_root: str, rows, violations, fixtures_total):
    totals = _compute_totals(rows)
    order = _group_order(rows)
    print("[token_ledger] 代码体积台账")
    print("合计: 本体 {files} 文件 / {loc} LOC / {bytes} bytes / ~{token} token".format(
        files=totals["files"], loc=fmt(totals["loc"]),
        bytes=fmt(totals["nbytes"]), token=fmt(totals["token"])))
    print("tests-fixtures(仅报量): {} LOC / {} bytes / ~{} token".format(
        fmt(fixtures_total["loc"]), fmt(fixtures_total["nbytes"]),
        fmt(fixtures_total["token"])))
    print()
    print("{:<42} {:>6} {:>12} {:>12} {:>12}".format("模块", "文件", "LOC", "bytes", "token"))
    for g in order:
        gr = [r for r in rows if r["group"] == g]
        print("{:<42} {:>6} {:>12} {:>12} {:>12}".format(
            g, len(gr), fmt(sum(r["loc"] for r in gr)),
            fmt(sum(r["nbytes"] for r in gr)), fmt(sum(r["token"] for r in gr))))
    print()
    if violations:
        print("[违规] 红线缺备书/拆分锚: {}".format(len(violations)))
        for rel, loc, desc in sorted(violations, key=lambda x: -x[1]):
            print("  - {rel} ({loc} 行): {desc}".format(rel=rel, loc=fmt(loc), desc=desc))
    else:
        print("[通过] 红线审计：无违规。")


# ---------------------------------------------------------------------------
# 入口
# ---------------------------------------------------------------------------

def parse_args(argv):
    ap = argparse.ArgumentParser(description="Vtuber_Dream 代码体积 / token 台账")
    ap.add_argument("--out", metavar="PATH", help="额外写完整 markdown 台账到该路径")
    ap.add_argument("--ci", action="store_true", help="CI 模式：有违规则退出码 1")
    ap.add_argument("--repo-root", metavar="DIR", help="仓库根（默认本脚本所在目录的上级）")
    return ap.parse_args(argv)


def default_repo_root():
    here = os.path.dirname(os.path.abspath(__file__))
    return os.path.dirname(here)


def main(argv=None):
    args = parse_args(argv if argv is not None else sys.argv[1:])
    repo_root = args.repo_root or default_repo_root()
    repo_root = os.path.abspath(repo_root)

    try:
        rows, violations, fixtures_total = collect(repo_root)
    except OSError as e:
        print("错误: {}".format(e), file=sys.stderr)
        return 2

    print_summary(repo_root, rows, violations, fixtures_total)

    if args.out:
        md = to_markdown(repo_root, rows, violations, fixtures_total)
        with open(args.out, "w", encoding="utf-8") as fh:
            fh.write(md)
        print("\n已写台账: {}".format(os.path.abspath(args.out)))

    if args.ci:
        return 1 if violations else 0
    return 0


if __name__ == "__main__":
    sys.exit(main())