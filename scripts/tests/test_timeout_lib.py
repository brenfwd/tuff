import os
import shutil
import subprocess
import tempfile
import unittest

LIB = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "lib", "timeout.sh")


class RunWithTimeout(unittest.TestCase):
    def run_lib(self, cmd, path=None):
        env = dict(os.environ)
        if path is not None:
            env["PATH"] = path
        return subprocess.run([shutil.which("bash"), "-c", f'. "{LIB}"; {cmd}'], env=env,
                              capture_output=True, text=True).returncode

    def test_kills_a_command_that_overruns(self):
        self.assertNotEqual(self.run_lib("run_with_timeout 1 sleep 30"), 0)

    def test_passes_through_the_exit_status(self):
        self.assertEqual(self.run_lib("run_with_timeout 5 true"), 0)
        self.assertEqual(self.run_lib("run_with_timeout 5 sh -c 'exit 7'"), 7)

    @unittest.skipUnless(shutil.which("perl"), "perl fallback needs perl")
    def test_falls_back_to_perl_without_timeout_or_gtimeout(self):
        with tempfile.TemporaryDirectory() as bin_dir:
            for tool in ("perl", "sleep", "true", "sh"):
                os.symlink(shutil.which(tool), os.path.join(bin_dir, tool))
            self.assertNotEqual(self.run_lib("run_with_timeout 1 sleep 30", bin_dir), 0)
            self.assertEqual(self.run_lib("run_with_timeout 5 true", bin_dir), 0)
            self.assertEqual(self.run_lib("run_with_timeout 5 sh -c 'exit 7'", bin_dir), 7)


if __name__ == "__main__":
    unittest.main()
