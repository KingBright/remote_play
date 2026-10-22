"""Synthetic readiness fixtures and payload proofs; no pairing, services, or permissions."""
import copy
import importlib.util
from pathlib import Path
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'acceptance/desktop_peer_probe.py'
spec = importlib.util.spec_from_file_location('desktop_peer_probe', SCRIPT)
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class DesktopPeerProbeTests(unittest.TestCase):
    def endpoints(self):
        group = dict(zip(probe.GROUP_KEYS, ('a' * 64, 'b' * 64)))
        transport = dict(state='connected', stage='active', http_status=101,
                         registrations_sent=1, pongs_received=1)
        net = dict(runtime_generation=1, relay_groups=group,
                   relay_control=transport, relay_discovery=transport,
                   owner_projection=dict(online_devices=1), gui_projection=dict(online_devices=1))
        b = dict(role='background-service', pid=1, executable_sha256='c' * 64,
                 discovered_routes=1, network_diagnostics=dict(network=copy.deepcopy(net)))
        g = dict(ok=True, pid=2, executable_sha256='c' * 64, network=copy.deepcopy(net))
        return b, g, group

    def test_healthy_transport_with_no_peer_never_passes(self):
        b, g, peer = self.endpoints()
        b['discovered_routes'] = 0
        result = probe.readiness(b, g, 'c' * 64, peer)
        self.assertFalse(result['ready_for_peer_verification'])
        self.assertIn('no_discovered_peer', result['reasons'])

    def test_distinct_groups_and_executables_are_not_accepted(self):
        b, g, peer = self.endpoints()
        g['network']['relay_groups']['control_sha256'] = 'd' * 64
        g['executable_sha256'] = 'e' * 64
        result = probe.readiness(b, g, 'c' * 64, peer)
        self.assertFalse(result['ready_for_peer_verification'])
        self.assertIn('executable_binding_mismatch', result['reasons'])

    def test_even_ready_endpoints_do_not_claim_a_native_transfer(self):
        b, g, peer = self.endpoints()
        result = probe.readiness(b, g, 'c' * 64, peer)
        self.assertTrue(result['ready_for_peer_verification'])
        self.assertFalse(result['native_transfer_verified'])
        self.assertFalse(probe.readiness(b, g, 'c' * 64)['ready_for_peer_verification'])

    def test_directional_payload_reuse_and_corruption(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve() / 'fixture'
            record = probe.prepare(root, 'book')
            path = Path(record['path'])
            self.assertEqual(probe.prepare(root, 'book')['sha256'], record['sha256'])
            with self.assertRaises(ValueError):
                probe.verify_content(path, 'studio')
            data = bytearray(path.read_bytes())
            data[-1] ^= 1
            path.write_bytes(data)
            with self.assertRaises(ValueError):
                probe.verify_content(path, 'book')
            with self.assertRaises(ValueError):
                probe.prepare(root, 'book')


if __name__ == '__main__':
    unittest.main()
