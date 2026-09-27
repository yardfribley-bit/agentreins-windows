import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "enroll-windows-audit-device.py"
SPEC = importlib.util.spec_from_file_location("audit_enrollment", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class EnrollmentTests(unittest.TestCase):
    def test_adds_device_without_replacing_existing_entries(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            server = root / "config.json"
            output = root / "windows-device.json"
            server.write_text(
                json.dumps({"admin": {"username": "admin"}, "devices": {"mac": {"label": "Mac"}}}),
                encoding="utf-8",
            )
            device_id = MODULE.enroll(
                server, output, "https://audit.example.com/api/audit/ingest", "yly029"
            )
            updated = json.loads(server.read_text(encoding="utf-8"))
            client = json.loads(output.read_text(encoding="utf-8"))
            self.assertIn("mac", updated["devices"])
            self.assertEqual(updated["devices"][device_id]["label"], "yly029")
            self.assertEqual(client["deviceID"], device_id)
            self.assertFalse(client["uploadEnabled"])
            self.assertEqual(updated["devices"][device_id]["enabledAt"], client["enabledAt"])
            self.assertNotIn(client["token"], server.read_text(encoding="utf-8"))
            self.assertEqual(len(list(root.glob("config.json.backup-*"))), 1)

    def test_refuses_to_overwrite_a_client_enrollment(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            server = root / "config.json"
            output = root / "device.json"
            server.write_text(json.dumps({"admin": {}, "devices": {}}), encoding="utf-8")
            output.write_text("existing enrollment", encoding="utf-8")
            with self.assertRaises(FileExistsError):
                MODULE.enroll(server, output, "https://audit.example.com/api/audit/ingest", "yly029")
            self.assertEqual(output.read_text(encoding="utf-8"), "existing enrollment")
            self.assertEqual(json.loads(server.read_text(encoding="utf-8"))["devices"], {})


if __name__ == "__main__":
    unittest.main()
