import copy
from datetime import datetime, timezone
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch
import urllib.error


MODULE_PATH = Path(__file__).resolve().parents[1] / "ci_report.py"
spec = importlib.util.spec_from_file_location("ci_report", MODULE_PATH)
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
TOKEN = "test-token-do-not-copy-into-a-report"


def environment(**changes):
    env = {
        "GITHUB_ACTIONS": "true", "GITHUB_EVENT_NAME": "push",
        "GITHUB_REPOSITORY": module.REPOSITORY,
        "GITHUB_REF": "refs/heads/m2-terrain", "GITHUB_SHA": "a" * 40,
        "GITHUB_RUN_ID": "123456", "GITHUB_RUN_ATTEMPT": "2",
        "CI_REPORT_LINUX_RESULT": "success", "CI_REPORT_WINDOWS_RESULT": "success",
        "CI_REPORT_SERVER_SCALE_RESULT": "success",
        "GITHUB_TOKEN": TOKEN,
    }
    env.update(changes)
    return env


def context(**changes):
    return module.Context.from_environment(environment(**changes))


def pr_value(ctx, number=17):
    return {
        "state": "open", "html_url": f"https://github.com/{module.REPOSITORY}/pull/{number}",
        "head": {"ref": ctx.branch, "sha": ctx.sha, "repo": {"full_name": module.REPOSITORY}},
        "base": {"ref": "main", "repo": {"full_name": module.REPOSITORY}},
    }


class Response:
    def __init__(self, value, status=200):
        self.status = status
        self.value = json.dumps(value).encode()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        pass

    def read(self, size):
        return self.value[:size]


class ScopeAndDeliveryTests(unittest.TestCase):
    def test_context_rejects_wrong_repo_event_branch_sha_and_results(self):
        changes = [
            {"GITHUB_ACTIONS": "false"}, {"GITHUB_EVENT_NAME": "pull_request"},
            {"GITHUB_REPOSITORY": "Astraneer/unrelated"},
            {"GITHUB_REF": "refs/heads/main"}, {"GITHUB_REF": "refs/heads/codex-ci-results"},
            {"GITHUB_REF": "refs/heads/m14-extra"}, {"GITHUB_REF": "refs/heads/m2-../escape"},
            {"GITHUB_REF": "refs/tags/m2-terrain"}, {"GITHUB_SHA": "../escape"},
            {"GITHUB_RUN_ID": "not-a-run"}, {"CI_REPORT_LINUX_RESULT": "green"},
            {"CI_REPORT_SERVER_SCALE_RESULT": ""}, {"CI_REPORT_SERVER_SCALE_RESULT": "green"},
        ]
        for change in changes:
            with self.subTest(change=change), self.assertRaises(module.ScopeError):
                context(**change)

    def test_report_preserves_actual_failed_cancelled_and_skipped_results(self):
        for result, expected in [("failure", "failure"), ("cancelled", "cancelled"), ("skipped", "incomplete")]:
            ctx = context(CI_REPORT_WINDOWS_RESULT=result)
            factory = Mock()
            report = module.make_report(ctx, module.delivery(ctx, Mock(), TOKEN, True, factory))
            self.assertEqual(report["checks"], {"linux": "success", "windows": result, "server_scale": "success"})
            self.assertEqual(report["conclusion"], expected)
            self.assertEqual(report["delivery"], {"status": "blocked_checks"})
            factory.assert_not_called()
            self.assertNotIn(TOKEN, json.dumps(report))

    def test_server_scale_is_required_for_report_success_and_delivery(self):
        for result, expected in [("failure", "failure"), ("cancelled", "cancelled"), ("skipped", "incomplete")]:
            with self.subTest(result=result):
                ctx = context(CI_REPORT_SERVER_SCALE_RESULT=result)
                factory = Mock()
                delivery = module.delivery(ctx, Mock(), TOKEN, True, factory)
                report = module.make_report(ctx, delivery)
                self.assertFalse(ctx.successful)
                self.assertEqual(report["schema_version"], 2)
                self.assertEqual(report["checks"], {"linux": "success", "windows": "success", "server_scale": result})
                self.assertEqual(report["conclusion"], expected)
                self.assertEqual(delivery, {"status": "blocked_checks"})
                factory.assert_not_called()
        env = environment()
        del env["CI_REPORT_SERVER_SCALE_RESULT"]
        with self.assertRaises(module.ScopeError):
            module.Context.from_environment(env)

    def test_advanced_branch_does_not_open_pr_for_untested_tip(self):
        relay = Mock()
        relay.remote_tip.return_value = "b" * 40
        factory = Mock()
        result = module.delivery(context(), relay, TOKEN, True, factory)
        self.assertEqual(result, {"status": "deferred", "reason": "branch_advanced"})
        factory.assert_not_called()

    def test_existing_scoped_pr_prevents_duplicate_post(self):
        ctx = context()
        opener = Mock(return_value=Response([pr_value(ctx)]))
        result = module.PullRequests(TOKEN, opener).ensure(ctx)
        self.assertEqual(result["status"], "existing")
        self.assertEqual(opener.call_count, 1)
        request = opener.call_args.args[0]
        self.assertEqual(request.method, "GET")
        self.assertTrue(request.full_url.startswith(f"https://api.github.com/repos/{module.REPOSITORY}/pulls?"))

    def test_create_pr_only_in_own_repo_with_actual_evidence(self):
        ctx = context()
        opener = Mock(side_effect=[Response([]), Response(pr_value(ctx), 201)])
        result = module.PullRequests(TOKEN, opener).ensure(ctx)
        self.assertEqual(result["status"], "created")
        request = opener.call_args_list[1].args[0]
        data = json.loads(request.data)
        self.assertEqual(request.method, "POST")
        self.assertEqual(request.full_url, f"https://api.github.com/repos/{module.REPOSITORY}/pulls")
        self.assertEqual(data["base"], "main")
        self.assertEqual(data["head"], "m2-terrain")
        self.assertIn(ctx.sha, data["body"])
        self.assertIn(ctx.run_url, data["body"])
        self.assertNotIn(TOKEN, data["body"])
        self.assertNotIn(TOKEN, json.dumps(result))

    def test_foreign_pr_response_is_rejected(self):
        ctx = context()
        value = pr_value(ctx)
        value["head"]["repo"]["full_name"] = "someone/else"
        with self.assertRaises(module.ApiError):
            module.PullRequests(TOKEN, Mock(return_value=Response([value]))).ensure(ctx)

    def test_denial_or_network_error_reports_status_without_credentials(self):
        ctx = context()
        relay = Mock()
        relay.remote_tip.return_value = ctx.sha
        for error, expected in [
            (urllib.error.HTTPError("https://api.github.com", 403, TOKEN, {}, io.BytesIO(TOKEN.encode())),
             {"status": "denied", "http_status": 403}),
            (urllib.error.URLError(TOKEN), {"status": "error"}),
        ]:
            opener = Mock(side_effect=error)
            factory = lambda token: module.PullRequests(token, opener)
            result = module.delivery(ctx, relay, TOKEN, True, factory)
            self.assertEqual(result, expected)
            self.assertNotIn(TOKEN, json.dumps(module.make_report(ctx, result)))

    def test_redirect_handler_never_forwards_authorization(self):
        handler = module.NoRedirect()
        self.assertIsNone(handler.redirect_request(None, None, 302, "Found", {}, "https://elsewhere.invalid"))

    def test_source_verification_checks_exact_checkout_and_origin(self):
        ctx = context()
        relay = module.GitRelay(MODULE_PATH.parents[1])
        values = [str(relay.root), ctx.sha, f"https://github.com/{module.REPOSITORY}.git"]
        relay.git = Mock(side_effect=[subprocess.CompletedProcess([], 0, value.encode(), b"") for value in values])
        relay.verify_source(ctx)
        for bad in [f"https://credential@github.com/{module.REPOSITORY}", "https://github.com/someone/else"]:
            relay.git = Mock(side_effect=[subprocess.CompletedProcess([], 0, value.encode(), b"")
                                         for value in [str(relay.root), ctx.sha, bad]])
            with self.assertRaises(module.ScopeError):
                relay.verify_source(ctx)


class GitRelayTests(unittest.TestCase):
    def setUp(self):
        cache = MODULE_PATH.parents[1] / ".local/ci-report-tests"
        cache.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=cache)
        self.base = Path(self.temp.name)
        self.remote = self.base / "remote.git"
        self.root = self.base / "source"
        self.run_cmd("git", "init", "--bare", str(self.remote))
        self.run_cmd("git", "init", "-b", "m2-terrain", str(self.root))
        self.git("config", "user.name", "CI relay test")
        self.git("config", "user.email", "relay-test@example.invalid")
        (self.root / ".gitignore").write_text(".local/\n")
        (self.root / "source.txt").write_text("original source\n")
        self.git("add", ".")
        self.git("commit", "-m", "local fixture source")
        self.git("remote", "add", "origin", str(self.remote))
        self.git("push", "origin", "m2-terrain")
        self.sha = self.git("rev-parse", "HEAD").stdout.decode().strip()
        self.relay = module.GitRelay(self.root)

    def tearDown(self):
        self.temp.cleanup()

    @staticmethod
    def run_cmd(*args):
        return subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)

    def git(self, *args):
        return self.run_cmd("git", "-C", str(self.root), *args)

    def report(self, sha=None):
        ctx = context(GITHUB_SHA=sha or self.sha)
        return module.make_report(ctx, {"status": "not_requested"}, datetime(2026, 10, 25, tzinfo=timezone.utc))

    def remote_read(self, path):
        return self.run_cmd("git", "--git-dir", str(self.remote), "show", f"{module.RESULTS_BRANCH}:{path}").stdout

    def test_publish_changes_only_reports_branch_and_is_idempotent(self):
        report = self.report()
        commit = self.relay.publish(report)
        data = json.loads(self.remote_read(f"reports/{self.sha}.json"))
        self.assertEqual(data, report)
        self.assertEqual(self.git("rev-parse", "HEAD").stdout.decode().strip(), self.sha)
        self.assertEqual(self.git("branch", "--show-current").stdout.decode().strip(), "m2-terrain")
        self.assertEqual(self.git("status", "--porcelain").stdout, b"")
        self.assertFalse((self.root / "reports").exists())
        self.assertEqual(self.relay.publish(report), commit)
        files = self.run_cmd("git", "--git-dir", str(self.remote), "ls-tree", "-r", "--name-only", module.RESULTS_BRANCH).stdout.decode().splitlines()
        self.assertEqual(files, [f"reports/{self.sha}.json"])

    def test_real_non_fast_forward_race_preserves_both_reports_without_force(self):
        second = self.base / "second"
        self.run_cmd("git", "clone", "--branch", "m2-terrain", str(self.remote), str(second))
        self.run_cmd("git", "-C", str(second), "config", "user.name", "Second relay test")
        self.run_cmd("git", "-C", str(second), "config", "user.email", "second@example.invalid")
        (second / "source.txt").write_text("new fixture source\n")
        self.run_cmd("git", "-C", str(second), "commit", "-am", "second fixture source")
        sha2 = self.run_cmd("git", "-C", str(second), "rev-parse", "HEAD").stdout.decode().strip()
        relay2 = module.GitRelay(second)
        push = self.relay._push
        races = []

        def race(commit):
            if not races:
                races.append(True)
                relay2.publish(self.report(sha2))
            return push(commit)

        with patch.object(self.relay, "_push", side_effect=race) as pushing:
            self.relay.publish(self.report())
            self.assertEqual(pushing.call_count, 2)
        self.assertEqual(json.loads(self.remote_read(f"reports/{self.sha}.json")), self.report())
        self.assertEqual(json.loads(self.remote_read(f"reports/{sha2}.json")), self.report(sha2))
        self.assertEqual(self.git("rev-parse", "HEAD").stdout.decode().strip(), self.sha)

    def test_unchanged_push_denial_does_not_loop_or_leak_stderr(self):
        failed = subprocess.CompletedProcess([], 1, b"", TOKEN.encode())
        with patch.object(self.relay, "_push", return_value=failed) as pushing:
            with self.assertRaises(module.GitError) as raised:
                self.relay.publish(self.report())
            self.assertEqual(pushing.call_count, 1)
            self.assertNotIn(TOKEN, str(raised.exception))

    def test_malformed_report_sha_cannot_write_a_path(self):
        value = copy.deepcopy(self.report())
        value["source"]["sha"] = "../escape"
        with self.assertRaises(module.ScopeError):
            self.relay.publish(value)


if __name__ == "__main__":
    unittest.main()
