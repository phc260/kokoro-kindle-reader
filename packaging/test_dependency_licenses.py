#!/usr/bin/env python3
"""Small offline regression fixtures for the dependency-notice tooling.

No application, no Rust build, no native tools, no network: every fixture is invented
here, and `cargo metadata` is replaced by a callback rather than run. Fixture notices are
INVENTED, never copied from a real dependency - a fixture built from real text passes
every test there is, which is exactly how it goes unnoticed.

Port of test-dependency-licenses.ps1, with one thing gone. That script could not import
the generator without executing it, so it parsed the generator's AST, found
`Get-LeadingCopyrightNotice` and `Add-PackagedLicenseAppendix` by name, and dot-sourced
their extents. That coupling is why the four scripts were treated as one indivisible unit.
Python imports them, so the tests exercise the production functions directly and the hack
is deleted rather than translated.
"""

import json
import os
import shutil
import sys
import tempfile
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from dotnet_compat import html_encode, sha256_text, write_all_text  # noqa: E402
import source_notices  # noqa: E402
import verify_dependency_licenses as verifier  # noqa: E402
from generate_dependency_licenses import (  # noqa: E402
    add_packaged_license_appendix, leading_copyright_notice,
)

passed = 0


def rejected(name, action):
    global passed
    try:
        action()
    except Exception:
        print("PASS: %s" % name)
        passed += 1
        return
    raise AssertionError("Unexpected success: %s" % name)


def main():
    global passed
    test_dir = Path(tempfile.gettempdir()) / ("kkr-license-test-" + uuid.uuid4().hex)
    test_dir = Path(os.path.abspath(test_dir))
    test_dir.mkdir()
    report = test_dir / "report.html"
    config = test_dir / "about.toml"

    def set_report(html):
        write_all_text(report, html)

    def check(cfg=None, notices=None):
        return lambda: verifier.verify(report, cfg or config, notices)

    try:
        # Invented notice with non-ASCII and HTML syntax: hashes must cover decoded UTF-8.
        notice = "Copyright © 2026 Fixture Authors <one & two>\nPermission notice.\n"
        h = sha256_text(notice)
        toml = ('[fixture.clarify]\nlicense = "MIT"\n[[fixture.clarify.git]]\n'
                'path = "LICENSE"\nchecksum = "%s"\n' % h)
        write_all_text(config, toml)
        inventory = '<span data-report-crate="fixture" data-report-version="1.0.0"></span>'
        section = ('<section class="resolved-license">'
                   '<span data-license-crate="fixture" data-license-version="1.0.0"></span>'
                   "<pre>" + html_encode(notice) + "</pre></section>")
        packaged = ('<h4 data-source-sha256="%s" data-text-sha256="%s">LICENSE</h4><pre>%s</pre>'
                    '<span hidden data-packaged-notice-count="1"></span>'
                    % (h, h, html_encode(notice)))
        valid = inventory + section + packaged

        set_report(valid)
        verifier.verify(report, config)
        print("PASS: exact UTF-8 notice and HTML round trip")
        passed += 1

        set_report(inventory + section.replace(html_encode(notice),
                                               "Copyright &lt;year&gt; &lt;owner&gt;") + packaged)
        rejected("canonical fallback after failed clarification", check())
        set_report(inventory + section.replace("Permission notice.", "Permission changed.") + packaged)
        rejected("changed notice text", check())
        set_report(inventory + section.replace('data-license-crate="fixture"',
                                               'data-license-crate="unrelated"') + packaged)
        rejected("another crate cannot supply the missing notice", check())
        set_report(valid + inventory.replace("1.0.0", "2.0.0"))
        rejected("one version cannot bless another version", check())
        set_report(inventory + packaged)
        rejected("missing licence sections", check())
        set_report(section + packaged)
        rejected("missing graph inventory", check())
        set_report(valid)
        write_all_text(config, toml.replace(h, "broken"))
        rejected("malformed configured checksum", check())

        # The production header extractor, imported rather than reconstructed.
        line_header = "// Copyright 2026 Fixture Authors\n// License terms and a second holder.\n"
        block_header = "/*\n * Copyright 2026 Fixture Authors\n * Full permission and disclaimer.\n */"
        for header in (line_header, block_header):
            if leading_copyright_notice(header + "\nfn example() {}\n") != header.rstrip():
                raise AssertionError("Leading notice was lost or changed.")
            passed += 1
        for source in ('const TEXT: &str = "Copyright 2026 Fiction";',
                       "//! Copyright in a documentation example\nfn example() {}\n",
                       "// Ordinary comment\nfn example() {}\n// Copyright later in code"):
            if leading_copyright_notice(source):
                raise AssertionError("Non-header text was harvested.")
            passed += 1

        # Exercise the complete appendix with invented embedded terms: Rust comments after
        # attributes/docs and a native .inl file, neither seen by the broad scan.
        package_dir = test_dir / "embedded"
        (package_dir / "src").mkdir(parents=True)
        (package_dir / "crypto").mkdir(parents=True)
        embedded_text = ("// Copyright 2026 Fixture Authors <one & two>\n"
                         "// Permission to use this invented fixture.\n// No warranty.")
        embedded_hash = sha256_text(embedded_text)
        rust_prefix = "#![allow(dead_code)]\n//! Documentation before the notice.\n\n"
        rust_path = package_dir / "src" / "lib.rs"
        write_all_text(rust_path,
                       (rust_prefix + embedded_text + "\nfn example() {}\n").replace("\n", "\r\n"))
        write_all_text(package_dir / "crypto" / "notice.inl", embedded_text + "\nint example;\n")
        # A BOM-bearing licence and an ordinary source header have no special clarification
        # requirement: the appendix's own integrity check must protect them.
        ordinary_license = ("Copyright 2026 Ordinary Fixture Authors\r\n"
                            "Invented permission and warranty terms.\r\n")
        with open(package_dir / "LICENSE", "wb") as f:
            f.write(b"\xef\xbb\xbf" + ordinary_license.encode("utf-8"))
        write_all_text(package_dir / "src" / "ordinary.rs",
                       "// Copyright 2026 Header Fixture Authors\n// Invented notice.\n"
                       "fn ordinary() {}\n")

        source_config = test_dir / "source-notices.json"
        entries = [
            {"crate": "embedded", "version": "1.0.0", "path": "src/lib.rs",
             "first_line": 4, "last_line": 6, "license": "ISC", "sha256": embedded_hash},
            {"crate": "embedded", "version": "1.0.0", "path": "crypto/notice.inl",
             "first_line": 1, "last_line": 3, "license": "ISC", "sha256": embedded_hash},
        ]
        write_all_text(source_config, json.dumps(entries))
        notices = source_notices.read_requirements(source_config)

        fixture_metadata = {
            "packages": [{"id": "embedded-id", "name": "embedded", "version": "1.0.0",
                          "manifest_path": str(package_dir / "Cargo.toml"),
                          "source": "registry+fixture", "authors": []}],
            "resolve": {"nodes": [{"id": "embedded-id"}]},
        }
        # Shadow the metadata command with a callback. No Cargo and no build runs.
        state = {"metadata": json.dumps(fixture_metadata)}
        fake_cargo = lambda _m, _t: state["metadata"]  # noqa: E731

        embedded_inventory = ('<span data-report-crate="embedded" '
                              'data-report-version="1.0.0"></span>')
        write_all_text(config, toml)
        set_report("<body>" + inventory + section + embedded_inventory + "</body>")
        added = add_packaged_license_appendix("fixture", "fixture-target", report, notices,
                                              run_cargo=fake_cargo)
        if added["files"] != 4:
            raise AssertionError("Both ordinary and embedded notices must be appended.")
        verifier.verify(report, config, source_config)
        embedded_valid = report.read_text(encoding="utf-8")
        passed += 1
        print("PASS: generator retains BOM/CRLF licence, ordinary header and embedded "
              "Rust/native notices")

        blocks = list(verifier.APPENDIX_TEXT.finditer(embedded_valid))
        license_block = next(b for b in blocks if b.group("label") == "LICENSE")
        header_block = next(b for b in blocks
                            if "leading comment excerpt" in b.group("label"))

        def mutate(old, new):
            return lambda: (set_report(embedded_valid.replace(old, new)),
                            verifier.verify(report, config, source_config))

        rejected("ordinary packaged licence changed while all special notices survive",
                 mutate(license_block.group(0),
                        license_block.group(0).replace(license_block.group("text"),
                                                       "NOTICE OMITTED")))
        rejected("ordinary source copyright changed",
                 mutate(header_block.group(0),
                        header_block.group(0).replace(header_block.group("text"),
                                                      "COPYRIGHT OMITTED")))
        rejected("whole ordinary notice block removed", mutate(license_block.group(0), ""))
        rejected("ordinary notice has no checkable text hash",
                 mutate(license_block.group(0),
                        license_block.group(0).replace("data-text-sha256=",
                                                       "data-ignored-sha256=")))
        rejected("duplicate appendix inventory",
                 lambda: (set_report(embedded_valid + '<span data-packaged-notice-count="4"></span>'),
                          verifier.verify(report, config, source_config)))
        rejected("missing appendix inventory",
                 mutate("data-packaged-notice-count=", "data-ignored-notice-count="))

        import re
        rejected("missing embedded notice despite valid package licence",
                 lambda: (set_report(re.sub(r'<section class="source-license">.*?</section>',
                                            "", embedded_valid, flags=re.S)),
                          verifier.verify(report, config, source_config)))
        rejected("changed embedded notice in extracted report",
                 mutate("No warranty.", "Truncated notice."))
        rejected("embedded notice attributed to another crate",
                 mutate('data-license-crate="embedded"', 'data-license-crate="unrelated"'))
        rejected("dependency upgrade requires source-notice review",
                 mutate("1.0.0", "2.0.0"))

        write_all_text(rust_path,
                       rust_prefix + embedded_text.replace("No warranty.", "Changed terms."))
        rejected("generator refuses source excerpt drift",
                 lambda: add_packaged_license_appendix("fixture", "fixture-target", report,
                                                       notices, run_cargo=fake_cargo))
        state["metadata"] = state["metadata"].replace("1.0.0", "2.0.0")
        rejected("generator refuses unreviewed dependency version",
                 lambda: add_packaged_license_appendix("fixture", "fixture-target", report,
                                                       notices, run_cargo=fake_cargo))

        print("==> PASS: %d offline dependency-notice checks" % passed)
    finally:
        # Delete only this test's freshly-created, fully resolved directory under TEMP.
        expected_parent = os.path.abspath(tempfile.gettempdir()).rstrip("\\/")
        import re as _re
        if (str(test_dir.parent).rstrip("\\/") != expected_parent or
                not _re.match(r"^kkr-license-test-[0-9a-f]{32}$", test_dir.name)):
            raise RuntimeError("Refusing cleanup outside the test directory: %s" % test_dir)
        shutil.rmtree(test_dir, ignore_errors=True)


if __name__ == "__main__":
    main()
