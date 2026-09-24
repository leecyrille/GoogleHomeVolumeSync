"""Generate THIRD-PARTY-NOTICES.txt for everything shipped in the Windows build.

Rust crates come from `cargo metadata` (normal dependencies reachable from the
app on x86_64-pc-windows-msvc); JavaScript packages are the runtime
dependencies in package.json. License files are copied from each package's
own directory; identical texts are printed once and listed against every
package that uses them.

Run from the repo root:  python tools/gen_notices.py
"""
import json
import pathlib
import re
import subprocess

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "THIRD-PARTY-NOTICES.txt"
LICENSE_FILE = re.compile(r"^(LICEN[CS]E|COPYING|NOTICE|UNLICENSE)([-_.].*)?$", re.IGNORECASE)

EXTRA = """\
Protocol definitions and references
===================================

Google Cast message format
  src-tauri/src/cast/proto.rs mirrors the CastMessage definition from
  Chromium's cast_channel.proto.
  Copyright 2013 The Chromium Authors. BSD-3-Clause:

  Redistribution and use in source and binary forms, with or without
  modification, are permitted provided that the following conditions are
  met:
     * Redistributions of source code must retain the above copyright
  notice, this list of conditions and the following disclaimer.
     * Redistributions in binary form must reproduce the above
  copyright notice, this list of conditions and the following disclaimer
  in the documentation and/or other materials provided with the
  distribution.
     * Neither the name of Google LLC nor the names of its
  contributors may be used to endorse or promote products derived from
  this software without specific prior written permission.

  THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
  "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
  LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
  A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
  OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
  SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
  LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
  DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
  THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
  (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
  OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

Roku External Control Protocol (ECP)
  The Roku backend is written from Roku's published ECP documentation:
  https://developer.roku.com/docs/developer-program/dev-tools/external-control-api.md

Yamaha Extended Control and LG webOS SSAP
  The Yamaha and LG backends follow the publicly documented Yamaha
  Extended Control (YXC) API and the LG webOS second-screen (SSAP)
  protocol. No third-party code is copied.

Calendar design
  public/calendar is the page from the PactoTech Calendar Saver
  (https://calendarsaver.com/, github.com/leecyrille/Windows-11-ICS-Calendar-Screensaver),
  by the same author, adapted for TVs.

Inter typeface (public/calendar/InterVariable.woff2)
  Copyright (c) 2016 The Inter Project Authors (https://github.com/rsms/inter)

  This Font Software is licensed under the SIL Open Font License, Version 1.1.
  This license is copied below, and is also available with a FAQ at:
  https://openfontlicense.org

  -----------------------------------------------------------
  SIL OPEN FONT LICENSE Version 1.1 - 26 February 2007
  -----------------------------------------------------------

  PREAMBLE
  The goals of the Open Font License (OFL) are to stimulate worldwide
  development of collaborative font projects, to support the font creation
  efforts of academic and linguistic communities, and to provide a free and
  open framework in which fonts may be shared and improved in partnership
  with others.

  The OFL allows the licensed fonts to be used, studied, modified and
  redistributed freely as long as they are not sold by themselves. The
  fonts, including any derivative works, can be bundled, embedded,
  redistributed and/or sold with any software provided that any reserved
  names are not used by derivative works. The fonts and derivatives,
  however, cannot be released under any other type of license. The
  requirement for fonts to remain under this license does not apply
  to any document created using the fonts or their derivatives.

  DEFINITIONS
  "Font Software" refers to the set of files released by the Copyright
  Holder(s) under this license and clearly marked as such. This may
  include source files, build scripts and documentation.

  "Reserved Font Name" refers to any names specified as such after the
  copyright statement(s).

  "Original Version" refers to the collection of Font Software components as
  distributed by the Copyright Holder(s).

  "Modified Version" refers to any derivative made by adding to, deleting,
  or substituting -- in part or in whole -- any of the components of the
  Original Version, by changing formats or by porting the Font Software to a
  new environment.

  "Author" refers to any designer, engineer, programmer, technical
  writer or other person who contributed to the Font Software.

  PERMISSION & CONDITIONS
  Permission is hereby granted, free of charge, to any person obtaining
  a copy of the Font Software, to use, study, copy, merge, embed, modify,
  redistribute, and sell modified and unmodified copies of the Font
  Software, subject to the following conditions:

  1) Neither the Font Software nor any of its individual components,
  in Original or Modified Versions, may be sold by itself.

  2) Original or Modified Versions of the Font Software may be bundled,
  redistributed and/or sold with any software, provided that each copy
  contains the above copyright notice and this license. These can be
  included either as stand-alone text files, human-readable headers or
  in the appropriate machine-readable metadata fields within text or
  binary files as long as those fields can be easily viewed by the user.

  3) No Modified Version of the Font Software may use the Reserved Font
  Name(s) unless explicit written permission is granted by the corresponding
  Copyright Holder. This restriction only applies to the primary font name as
  presented to the users.

  4) The name(s) of the Copyright Holder(s) and the Author(s) of the Font
  Software shall not be used to promote, endorse or advertise any
  Modified Version, except to acknowledge the contribution(s) of the
  Copyright Holder(s) and the Author(s) or with their explicit written
  permission.

  5) The Font Software, modified or unmodified, in part or in whole,
  must be distributed entirely under this license, and must not be
  distributed under any other license. The requirement for fonts to
  remain under this license does not apply to any document created
  using the Font Software.

  TERMINATION
  This license becomes null and void if any of the above conditions are
  not met.

  DISCLAIMER
  THE FONT SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
  EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO ANY WARRANTIES OF
  MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT
  OF COPYRIGHT, PATENT, TRADEMARK, OR OTHER RIGHT. IN NO EVENT SHALL THE
  COPYRIGHT HOLDER BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
  INCLUDING ANY GENERAL, SPECIAL, INDIRECT, INCIDENTAL, OR CONSEQUENTIAL
  DAMAGES, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
  FROM, OUT OF THE USE OR INABILITY TO USE THE FONT SOFTWARE OR FROM
  OTHER DEALINGS IN THE FONT SOFTWARE.

"""


def license_texts(pkg_dir: pathlib.Path) -> list[tuple[str, str]]:
    texts = []
    if not pkg_dir.is_dir():
        return texts
    for f in sorted(pkg_dir.iterdir()):
        if f.is_file() and LICENSE_FILE.match(f.name):
            try:
                texts.append((f.name, f.read_text(encoding="utf-8", errors="replace").strip()))
            except OSError:
                pass
    return texts


def rust_packages() -> list[dict]:
    meta = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--filter-platform", "x86_64-pc-windows-msvc"],
        cwd=ROOT / "src-tauri",
    ))
    pkgs = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root = meta["resolve"]["root"]
    seen, stack = set(), [root]
    while stack:
        nid = stack.pop()
        if nid in seen:
            continue
        seen.add(nid)
        for dep in nodes[nid]["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    seen.discard(root)
    out = []
    for pid in seen:
        p = pkgs[pid]
        out.append({
            "name": p["name"], "version": p["version"],
            "license": p.get("license") or "see license file",
            "url": p.get("repository") or p.get("homepage") or "",
            "texts": license_texts(pathlib.Path(p["manifest_path"]).parent),
        })
    return sorted(out, key=lambda p: (p["name"].lower(), p["version"]))


def js_packages() -> list[dict]:
    pkg = json.loads((ROOT / "package.json").read_text(encoding="utf-8"))
    out = []
    for name in sorted(pkg.get("dependencies", {})):
        d = ROOT / "node_modules" / name
        meta = json.loads((d / "package.json").read_text(encoding="utf-8"))
        repo = meta.get("repository")
        url = repo.get("url", "") if isinstance(repo, dict) else (repo or meta.get("homepage", ""))
        out.append({
            "name": name, "version": meta.get("version", ""),
            "license": meta.get("license", "see license file"),
            "url": url, "texts": license_texts(d),
        })
    return out


def main() -> None:
    rust, js = rust_packages(), js_packages()
    everything = [("Rust crate", p) for p in rust] + [("npm package", p) for p in js]

    # Identical license texts are printed once.
    by_text: dict[str, list[str]] = {}
    order: list[str] = []
    missing: list[str] = []
    for kind, p in everything:
        label = f"{p['name']} {p['version']}"
        if not p["texts"]:
            missing.append(f"{label} ({p['license']})")
        for _, text in p["texts"]:
            if text not in by_text:
                by_text[text] = []
                order.append(text)
            if label not in by_text[text]:
                by_text[text].append(label)

    lines = [
        "Unofficial Google Home Volume Sync - third-party notices",
        "=" * 56,
        "",
        "This application includes open-source software. Each component is",
        "listed below with its license, followed by the full license texts.",
        "Thank you to everyone who built and maintains these projects.",
        "",
        EXTRA,
        f"Components ({len(everything)})",
        "=" * 20,
        "",
    ]
    for kind, p in everything:
        url = f"  {p['url']}" if p["url"] else ""
        lines.append(f"{p['name']} {p['version']}  [{kind}]  {p['license']}{url}")
    if missing:
        lines += ["", "No license file shipped in the package (license per its manifest):"]
        lines += [f"  {m}" for m in missing]
    lines += ["", "", "License texts", "=" * 13, ""]
    for text in order:
        users = by_text[text]
        lines.append("-" * 72)
        lines.append("Used by: " + ", ".join(users))
        lines.append("-" * 72)
        lines.append(text)
        lines.append("")
    OUT.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"{OUT.name}: {len(everything)} components, {len(order)} distinct license texts, "
          f"{OUT.stat().st_size // 1024} KB")


if __name__ == "__main__":
    main()
