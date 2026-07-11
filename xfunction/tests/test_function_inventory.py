import os
import re
import unittest


class TestFunctionInventory(unittest.TestCase):

    def test_verifier_references_deployed_function_name(self):
        root = os.path.dirname(os.path.dirname(__file__))
        with open(os.path.join(root, "function_app.py"), encoding="utf-8") as handle:
            app_source = handle.read()
        deployed = set(re.findall(r'@app\.function_name\(name="([^"]+)"\)', app_source))

        with open(
            os.path.join(root, "scripts", "verify-deployment.ps1"),
            encoding="utf-8",
        ) as handle:
            verifier = handle.read()

        referenced = set(re.findall(r'DirectVaultRbacProcessor|VaultRbacProcessor', verifier))
        self.assertEqual(referenced, {"DirectVaultRbacProcessor"})
        self.assertTrue(referenced.issubset(deployed))


if __name__ == "__main__":
    unittest.main()
