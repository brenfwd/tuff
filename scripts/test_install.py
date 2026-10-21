import os
import subprocess
import unittest

SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "install.sh")


def run(*args, env=None):
    full = {k: v for k, v in os.environ.items() if k != "TUFF_ACCEPT_MOJANG_EULA"}
    full.update(env or {})
    return subprocess.run(
        ["bash", SCRIPT, *args],
        capture_output=True,
        text=True,
        stdin=subprocess.DEVNULL,
        env=full,
    )


class InstallScriptTest(unittest.TestCase):
    def test_refuses_without_eula(self):
        r = run("--dry-run")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("EULA", r.stderr)
        self.assertNotIn("cargo build", r.stdout)

    def test_flag_accepts_eula_and_plans_build(self):
        r = run("--dry-run", "--accept-eula", "--prefix", "/tmp/x", "--jobs", "3", "--clean")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("[dry-run] cargo build --profile release --locked --bin tuff", r.stdout)
        self.assertIn("/tmp/x/tuff", r.stdout)
        self.assertIn("rm -rf", r.stdout)

    def test_opt_uses_release_opt_profile(self):
        r = run("--dry-run", "--accept-eula", "--no-update", "--opt", "--prefix", "/tmp/x")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("[dry-run] cargo build --profile release-opt --locked --bin tuff", r.stdout)
        self.assertIn("target/release-opt/tuff", r.stdout)

    def test_target_same_as_host_builds_natively(self):
        host = subprocess.run(["rustc", "-vV"], capture_output=True, text=True).stdout
        host = [l.split()[1] for l in host.splitlines() if l.startswith("host:")][0]
        r = run("--dry-run", "--accept-eula", "--no-update", "--target", host, "--prefix", "/tmp/x")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("[dry-run] cargo build --profile release --locked --bin tuff", r.stdout)
        self.assertNotIn("zigbuild", r.stdout)

    def test_cross_target_uses_zigbuild(self):
        t = "aarch64-unknown-linux-musl"
        r = run("--dry-run", "--accept-eula", "--no-update", "--opt", "--target", t, "--prefix", "/tmp/x")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("[dry-run] rustup target add " + t, r.stdout)
        self.assertIn("[dry-run] cargo zigbuild --profile release-opt --locked --target " + t + " --bin tuff", r.stdout)
        self.assertIn("target/" + t + "/release-opt/tuff", r.stdout)

    def test_env_accepts_eula(self):
        r = run("--dry-run", "--no-update", env={"TUFF_ACCEPT_MOJANG_EULA": "1"})
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn("git pull", r.stdout)
        self.assertNotIn("rm -rf", r.stdout)

    def test_rejects_bad_arguments(self):
        for args in (["--bogus"], ["--accept-eula", "--jobs", "x"], ["--accept-eula", "--jobs", "0"]):
            self.assertNotEqual(run("--dry-run", *args).returncode, 0, args)

    def test_help(self):
        r = run("--help")
        self.assertEqual(r.returncode, 0)
        self.assertIn("--accept-eula", r.stdout)


if __name__ == "__main__":
    unittest.main()
