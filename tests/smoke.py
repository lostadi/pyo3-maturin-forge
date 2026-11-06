#!/usr/bin/env python3
"""Exercise the compiled CLI against disposable sources, optionally through a wheel."""

import argparse
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import venv


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--wheel", action="store_true", help="also build and import a real wheel; requires maturin")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)

    with tempfile.TemporaryDirectory(prefix="forge-smoke-") as temporary:
        work = Path(temporary)
        project = work / "python-project"
        project.mkdir()
        source = 'def greet(args):\n    return "hello " + ", ".join(args)\n\nif __name__ == "__main__":\n    print("payload-executed")\n'
        (project / "demo.py").write_text(source)
        (project / "imports.py").write_text("import json\nimport torch\nfrom transformers import AutoModel\n")
        (project / "loop").symlink_to(project, target_is_directory=True)
        external = work / "private.py"
        external.write_text("raise RuntimeError('must not be copied')\n")
        (project / "outside.py").symlink_to(external)

        def invoke(*options):
            result = subprocess.run([str(binary), *options], cwd=work, text=True,
                                    capture_output=True, timeout=180, check=True)
            return result.stdout

        scan = invoke("scan", "--root", str(project))
        assert "python files: 2" in scan, scan
        assert "heavy modules seen: torch, transformers" in scan, scan
        invoke("init", "--root", str(project), "--package", "fixture_bridge", "--copy-python")
        generated = project / ".pyo3-forge"
        payload = generated / "python/fixture_bridge/payload"
        assert generated.joinpath("Cargo.toml").is_file()
        assert not work.joinpath(".pyo3-forge").exists()
        assert payload.joinpath("demo.py").read_text() == source
        assert sorted(p.name for p in payload.iterdir()) == ["demo.py", "imports.py"]
        assert project.joinpath("demo.py").read_text() == source

        # An explicit generated directory must not copy its own Python wrapper.
        invoke("init", "--forge-dir", "generated", "--root", str(project),
               "--package", "custom_bridge", "--copy-python")
        custom = project / "generated/python/custom_bridge/payload"
        assert sorted(p.name for p in custom.iterdir()) == ["demo.py", "imports.py"]
        checked = json.loads(invoke("check", "--root", str(project),
                                    "--python", sys.executable, "--module", "json"))
        assert checked["json"]["ok"] is True
        print("PASS: import scan, symlink exclusion, output placement, payload copy, and import report")

        if args.wheel:
            invoke("build", "--root", str(project), "--python", sys.executable, "--debug")
            wheels = list(generated.joinpath("target/wheels").glob("*.whl"))
            assert len(wheels) == 1, wheels
            environment = work / "venv"
            venv.create(environment, with_pip=True)
            python = environment / ("Scripts/python.exe" if sys.platform == "win32" else "bin/python")
            subprocess.run([str(python), "-m", "pip", "install", "--no-deps", str(wheels[0])], check=True)
            code = '''import fixture_bridge as forge
assert forge.import_check(["json"])[0][1] is True
assert forge.import_check(["forge_missing_module_123"])[0][1] is False
assert forge.call_payload_object("demo:greet", ["world"]) == "'hello world'"
forge.run_payload_module("demo")
print("PASS: wheel import, native bridge, payload call, payload execution")
'''
            subprocess.run([str(python), "-c", code], cwd=work, check=True)


if __name__ == "__main__":
    main()
