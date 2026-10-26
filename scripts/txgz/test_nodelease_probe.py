"""Safety/control tests with fake APIs, commands, signals, and clock only."""
import copy
import unittest
from unittest.mock import patch

import nodelease_probe as probe

SPEC = {
    "master": probe.MASTER, "node_id": "txgz-a",
    "device_id": "00000000-0000-0000-0000-000000000001", "public_key": "fixture-key",
    "ipv4": "10.88.0.10/32", "ipv6": "fd88:1000::a/128",
    "admin_token": "test-only-admin", "client_token": "test-only-client",
    "nodes": [{"id": "txgz-a", "interface": "pmman-dev-a", "route_table": "51830"},
              {"id": "txgz-b", "interface": "pmman-dev-b", "route_table": "51831"}],
}
ACTIVE = [{"device_id": SPEC["device_id"], "public_key": SPEC["public_key"], "status": "active"}]


def observation(present=False, up=False):
    result = []
    for node in SPEC["nodes"]:
        owns = present and node["id"] == "txgz-a"
        result.append({"id": node["id"], "interface": node["interface"], "up": up,
                       "peers": [SPEC["public_key"]] if owns else [],
                       "routes4": [{"dst": "10.88.0.10", "dev": node["interface"]}] if owns else [],
                       "routes6": [{"dst": "fd88:1000::a", "dev": node["interface"]}] if owns else []})
    return result


class Process:
    fd = 999

    def __init__(self):
        self.events = []

    def stop(self):
        self.events.append("STOP")

    def resume(self):
        self.events.append("CONT")

    def close(self):
        self.events.append("close")


class Guard:
    def __init__(self):
        self.canceled = False

    def cancel(self):
        self.canceled = True


class Clock:
    now = 100

    def monotonic(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


class LeaseProbeTests(unittest.TestCase):
    def test_bounded_pause_resumes_exact_process_after_real_assertions(self):
        process, guard, clock = Process(), Guard(), Clock()
        with patch.object(probe.time, "monotonic", clock.monotonic), patch.object(probe.time, "sleep", clock.sleep), patch.object(probe, "snapshot", return_value=observation()):
            evidence = probe.pause_and_observe(SPEC, process, guard)
        self.assertEqual(process.events, ["STOP", "CONT", "close"])
        self.assertTrue(guard.canceled)
        self.assertEqual(clock.now, 140)
        self.assertTrue(probe.failclosed(evidence[0]["nodes"], SPEC))

    def test_command_timeout_or_assertion_interrupt_always_resumes_master(self):
        for error in (TimeoutError("command timeout"), RuntimeError("termination")):
            process, guard, clock = Process(), Guard(), Clock()
            with patch.object(probe.time, "monotonic", clock.monotonic), patch.object(probe.time, "sleep", clock.sleep), patch.object(probe, "snapshot", side_effect=error):
                with self.assertRaises(type(error)):
                    probe.pause_and_observe(SPEC, process, guard)
            self.assertEqual(process.events, ["STOP", "CONT", "close"])
            self.assertTrue(guard.canceled)

    def test_other_active_lease_prevents_any_pause(self):
        foreign = {"device_id": "other", "public_key": "other", "status": "active"}
        with patch.object(probe, "request", return_value=ACTIVE + [foreign]), patch.object(probe, "MasterProcess") as process:
            with self.assertRaisesRegex(RuntimeError, "other active"):
                probe.run(SPEC)
            process.assert_not_called()

    def test_other_kernel_peer_prevents_any_pause(self):
        before = observation(present=True, up=True)
        before[1]["peers"] = ["unowned-key"]
        with patch.object(probe, "request", return_value=ACTIVE), patch.object(probe, "snapshot", return_value=before), patch.object(probe, "MasterProcess") as process:
            with self.assertRaisesRegex(RuntimeError, "other kernel"):
                probe.run(SPEC)
            process.assert_not_called()

    def test_unrelated_interface_is_rejected_before_api_or_signals(self):
        spec = copy.deepcopy(SPEC)
        spec["nodes"][0]["interface"] = "wg0"
        with patch.object(probe, "request") as request, patch.object(probe, "MasterProcess") as process:
            with self.assertRaises(ValueError):
                probe.run(spec)
            request.assert_not_called()
            process.assert_not_called()

    def test_route_assertions_match_owned_host_routes_and_interface(self):
        leaked = observation(present=True)
        leaked[0]["peers"] = []
        self.assertFalse(probe.failclosed(leaked, SPEC))
        clean = observation()
        clean[0]["routes4"] = [{"dst": "10.88.0.0/24", "dev": "pmman-dev-a"},
                              {"dst": "10.88.0.10/32", "dev": "unrelated"}]
        self.assertTrue(probe.failclosed(clean, SPEC))

    def test_client_lease_renews_before_and_after_pause_without_re_registration(self):
        process, guard, clock = Process(), Guard(), Clock()
        with patch.object(probe, "request", side_effect=[ACTIVE, None, ACTIVE, None]) as request, patch.object(probe, "snapshot", side_effect=[observation(True, True), observation()]), patch.object(probe, "MasterProcess", return_value=process), patch.object(probe, "ResumeGuard", return_value=guard), patch.object(probe.time, "monotonic", clock.monotonic), patch.object(probe.time, "sleep", clock.sleep):
            result = probe.run(SPEC)
        self.assertTrue(result["passed"] and result["clientLeaseRenewed"])
        paths = [call.args[0] for call in request.call_args_list]
        self.assertEqual(paths.count("/v1/devices/" + SPEC["device_id"] + "/heartbeat"), 2)
        self.assertNotIn("/v1/client/connections", paths)
        self.assertEqual(process.events, ["STOP", "CONT", "close"])

    def test_observation_mode_never_authenticates_or_pauses(self):
        spec = dict(SPEC, mode="observe")
        with patch.object(probe, "request") as request, patch.object(probe, "MasterProcess") as process, patch.object(probe, "snapshot", return_value=observation(True, True)):
            self.assertTrue(probe.run(spec)["passed"])
            request.assert_not_called()
            process.assert_not_called()


if __name__ == "__main__":
    unittest.main()
