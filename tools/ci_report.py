#!/usr/bin/env python3
"""Relay actual Actions outcomes through this repository's Git transport.

The running workflow supplies trusted context and `needs.*.result`. This script
does not infer a successful test result from a build, a branch, or a PR. It never
checks out the reports branch and never puts credentials in Git URLs or reports.
"""
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
import json
import os
import re
import subprocess
import tempfile
import urllib.error
import urllib.parse
import urllib.request


REPOSITORY = "Astraneer/cubi-cyber-v1"
RESULTS_BRANCH = "codex-ci-results"
SHA = re.compile(r"[0-9a-f]{40}\Z")
STAGE_BRANCH = re.compile(r"m(0|[1-9]|1[0-3])-[a-z0-9]+(?:-[a-z0-9]+)*\Z")
JOB_RESULTS = frozenset({"success", "failure", "cancelled", "skipped"})
PR_URL = re.compile(r"https://github\.com/Astraneer/cubi-cyber-v1/pull/[1-9][0-9]*\Z")

STAGES = {
    0: ("Основа Rust и native/WebGPU клиента", "Общий Rust workspace, программный рендер и проверяемый запуск native/WebGPU клиента."),
    1: ("Страницы мира и асинхронная генерация", "Версионированные страницы мира, транзакции, журнал и асинхронный генератор."),
    2: ("Terrain, мешинг и GPU-отсечение", "Страничный мешинг и общий terrain-рендер с MSAA для native и браузера."),
    3: ("Движение игрока и правки мира", "Физика игрока, камера, полёт и интерактивное ломание/установка блоков."),
    4: ("Свет и стабильные тени", "Игровой свет и рендер теней со стабилизацией и кэшем."),
    5: ("LOD фактического мира", "Дальний мир строится по версионированным сводкам, учитывающим правки."),
    6: ("Авторитетный сервер, сеть и сохранения", "Серверная проверка действий, репликация страниц и восстановление сохранённого мира."),
    7: ("Сущности и PvP", "Инстансинг сущностей, поведение мобов и серверные правила PvP."),
    8: ("Выживание и интерактивные блоки", "Здоровье, еда, жидкости, ферма и блоки с содержимым."),
    9: ("Mekanism и логистика", "Машины, энергия и событийная логистика с проверками поведения по тикам."),
    10: ("Атмосфера, вода и отражённый свет", "Небо, облака, вода и освещение в общем графе кадра."),
    11: ("Интерфейс и команды", "Общий интерфейс native/WebGPU, инвентарь, NEI, команды и роли."),
    12: ("Звук и локальный импорт ресурсов", "Позиционный звук и локальный импорт пользовательских ресурсов."),
    13: ("Сборки и маршрут измерений", "Воспроизводимый маршрут измерений, F3/F8 и сборки для запуска заказчиком."),
}


class ScopeError(ValueError):
    """Invalid or out-of-scope context; its message never includes input values."""


class GitError(RuntimeError):
    def __init__(self, operation, code):
        self.operation = operation
        self.code = code
        super().__init__(f"{operation} failed (exit {code})")


@dataclass(frozen=True)
class Context:
    sha: str
    branch: str
    stage: int
    run_id: str
    run_attempt: str
    linux: str
    windows: str
    server_scale: str

    def __post_init__(self):
        match = STAGE_BRANCH.fullmatch(self.branch)
        if (not SHA.fullmatch(self.sha) or not match or int(match[1]) != self.stage
                or self.linux not in JOB_RESULTS or self.windows not in JOB_RESULTS
                or self.server_scale not in JOB_RESULTS
                or not re.fullmatch(r"[1-9][0-9]*", self.run_id)
                or not re.fullmatch(r"[1-9][0-9]*", self.run_attempt)):
            raise ScopeError("invalid_context_fields")

    @classmethod
    def from_environment(cls, environ):
        if environ.get("GITHUB_ACTIONS") != "true" or environ.get("GITHUB_EVENT_NAME") != "push":
            raise ScopeError("runner_push_required")
        if environ.get("GITHUB_REPOSITORY") != REPOSITORY:
            raise ScopeError("repository_out_of_scope")
        ref = environ.get("GITHUB_REF", "")
        if not ref.startswith("refs/heads/"):
            raise ScopeError("branch_ref_required")
        branch = ref.removeprefix("refs/heads/")
        match = STAGE_BRANCH.fullmatch(branch)
        if not match:
            raise ScopeError("branch_out_of_scope")
        sha = environ.get("GITHUB_SHA", "")
        if not SHA.fullmatch(sha):
            raise ScopeError("invalid_source_sha")
        run_id = environ.get("GITHUB_RUN_ID", "")
        attempt = environ.get("GITHUB_RUN_ATTEMPT", "")
        if not re.fullmatch(r"[1-9][0-9]*", run_id) or not re.fullmatch(r"[1-9][0-9]*", attempt):
            raise ScopeError("invalid_run_identity")
        linux = environ.get("CI_REPORT_LINUX_RESULT", "")
        windows = environ.get("CI_REPORT_WINDOWS_RESULT", "")
        server_scale = environ.get("CI_REPORT_SERVER_SCALE_RESULT", "")
        if any(result not in JOB_RESULTS for result in (linux, windows, server_scale)):
            raise ScopeError("invalid_job_result")
        return cls(sha, branch, int(match[1]), run_id, attempt, linux, windows, server_scale)

    @property
    def successful(self):
        return self.linux == self.windows == self.server_scale == "success"

    @property
    def run_url(self):
        return f"https://github.com/{REPOSITORY}/actions/runs/{self.run_id}"


def make_report(context, delivery, now=None):
    if context.successful:
        conclusion = "success"
    elif "failure" in (context.linux, context.windows, context.server_scale):
        conclusion = "failure"
    elif "cancelled" in (context.linux, context.windows, context.server_scale):
        conclusion = "cancelled"
    else:
        conclusion = "incomplete"
    timestamp = now or datetime.now(timezone.utc)
    return {
        "schema_version": 2,
        "repository": REPOSITORY,
        "source": {"sha": context.sha, "branch": context.branch, "stage": f"M{context.stage}"},
        "workflow": {"name": "Acceptance", "run_id": context.run_id,
                     "run_attempt": context.run_attempt, "url": context.run_url},
        "checks": {"linux": context.linux, "windows": context.windows,
                   "server_scale": context.server_scale},
        "conclusion": conclusion,
        "delivery": delivery,
        "reported_at": timestamp.isoformat(),
    }


class GitRelay:
    def __init__(self, root, attempts=5):
        self.root = Path(root).resolve()
        self.attempts = attempts

    def git(self, *args, data=None, extra_env=None, check=True):
        env = os.environ.copy()
        env["GIT_TERMINAL_PROMPT"] = "0"
        if extra_env:
            env.update(extra_env)
        try:
            result = subprocess.run(["git", "-C", str(self.root), *args], input=data,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, timeout=60)
        except subprocess.TimeoutExpired:
            raise GitError(args[0], 124) from None
        if check and result.returncode:
            # Git errors can include authentication headers or URLs on some
            # transports. Expose only operation/status, never captured output.
            raise GitError(args[0], result.returncode)
        return result

    def verify_source(self, context):
        root = self.git("rev-parse", "--show-toplevel").stdout.decode().strip()
        if Path(root).resolve() != self.root:
            raise ScopeError("checkout_root_mismatch")
        head = self.git("rev-parse", "--verify", "HEAD^{commit}").stdout.decode().strip()
        if head != context.sha:
            raise ScopeError("checkout_sha_mismatch")
        remote = self.git("remote", "get-url", "origin").stdout.decode().strip()
        # checkout@v4 uses this HTTPS URL and supplies auth through its persisted
        # Git header. Do not read that header or reconstruct credentials.
        parsed = urllib.parse.urlsplit(remote)
        if (parsed.scheme != "https" or parsed.netloc != "github.com" or parsed.username
                or parsed.password or parsed.query or parsed.fragment
                or parsed.path.removesuffix(".git").strip("/") != REPOSITORY):
            raise ScopeError("origin_out_of_scope")

    def remote_tip(self, branch):
        if branch != RESULTS_BRANCH and not STAGE_BRANCH.fullmatch(branch):
            raise ScopeError("remote_branch_out_of_scope")
        output = self.git("ls-remote", "--heads", "origin", f"refs/heads/{branch}").stdout.decode()
        lines = output.splitlines()
        if not lines:
            return None
        if len(lines) != 1:
            raise ScopeError("ambiguous_remote_ref")
        fields = lines[0].split()
        if len(fields) != 2 or not SHA.fullmatch(fields[0]) or fields[1] != f"refs/heads/{branch}":
            raise ScopeError("invalid_remote_ref")
        return fields[0]

    def _push(self, commit):
        return self.git("push", "origin", f"{commit}:refs/heads/{RESULTS_BRANCH}", check=False)

    def publish(self, report):
        sha = report.get("source", {}).get("sha", "")
        if not SHA.fullmatch(sha) or report.get("repository") != REPOSITORY:
            raise ScopeError("invalid_report_identity")
        path = f"reports/{sha}.json"
        payload = (json.dumps(report, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode()
        cache = self.root / ".local/ci-report"
        cache.mkdir(parents=True, exist_ok=True)
        local = cache / path
        local.parent.mkdir(parents=True, exist_ok=True)
        local.write_bytes(payload)
        blob = self.git("hash-object", "-w", "--stdin", data=payload).stdout.decode().strip()
        for _ in range(self.attempts):
            base = self.remote_tip(RESULTS_BRANCH)
            if base:
                self.git("fetch", "--no-tags", "--depth=1", "origin", f"refs/heads/{RESULTS_BRANCH}")
                base = self.git("rev-parse", "--verify", "FETCH_HEAD^{commit}").stdout.decode().strip()
                existing = self.git("show", f"{base}:{path}", check=False)
                if existing.returncode == 0 and existing.stdout == payload:
                    return base
            with tempfile.TemporaryDirectory(prefix="index-", dir=cache) as directory:
                env = {"GIT_INDEX_FILE": str(Path(directory) / "index")}
                self.git("read-tree", base if base else "--empty", extra_env=env)
                self.git("update-index", "--add", "--cacheinfo", "100644", blob, path, extra_env=env)
                tree = self.git("write-tree", extra_env=env).stdout.decode().strip()
                env.update({
                    "GIT_AUTHOR_NAME": "github-actions[bot]",
                    "GIT_AUTHOR_EMAIL": "41898282+github-actions[bot]@users.noreply.github.com",
                    "GIT_COMMITTER_NAME": "github-actions[bot]",
                    "GIT_COMMITTER_EMAIL": "41898282+github-actions[bot]@users.noreply.github.com",
                })
                parents = ["-p", base] if base else []
                commit = self.git("commit-tree", tree, *parents,
                                  data=f"Record Acceptance results for {sha}\n".encode(),
                                  extra_env=env).stdout.decode().strip()
            pushed = self._push(commit)
            if pushed.returncode == 0:
                return commit
            # Retry only after evidence of a competing update; permission or
            # unchanged network failures are not repeated five times.
            if self.remote_tip(RESULTS_BRANCH) == base:
                raise GitError("push_rejected", pushed.returncode)
        raise GitError("concurrent_push_limit", 1)


class ApiError(RuntimeError):
    def __init__(self, status=None):
        self.status = status
        super().__init__("GitHub API request failed")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, file, code, message, headers, new_url):
        # Never forward Authorization to another host or a renamed repository.
        return None


class PullRequests:
    def __init__(self, token, opener=None):
        self._token = token
        self._opener = opener or urllib.request.build_opener(NoRedirect()).open

    def request(self, method, path, data=None):
        url = f"https://api.github.com/repos/{REPOSITORY}/pulls{path}"
        body = json.dumps(data, ensure_ascii=False).encode() if data is not None else None
        request = urllib.request.Request(url, data=body, method=method, headers={
            "Authorization": f"Bearer {self._token}",
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "Content-Type": "application/json",
        })
        try:
            with self._opener(request, timeout=20) as response:
                if response.status not in (200, 201):
                    raise ApiError(response.status)
                payload = response.read(2_000_001)
                if len(payload) > 2_000_000:
                    raise ApiError()
                return json.loads(payload)
        except urllib.error.HTTPError as error:
            raise ApiError(error.code) from None
        except (urllib.error.URLError, ValueError, OSError):
            raise ApiError() from None

    @staticmethod
    def validate_pr(value, context):
        if not isinstance(value, dict):
            raise ApiError()
        base, head = value.get("base"), value.get("head")
        if not isinstance(base, dict) or not isinstance(head, dict):
            raise ApiError()
        base_repo, head_repo = base.get("repo"), head.get("repo")
        if not isinstance(base_repo, dict) or not isinstance(head_repo, dict):
            raise ApiError()
        url = value.get("html_url", "")
        if (not isinstance(url, str) or not PR_URL.fullmatch(url) or value.get("state") != "open"
                or base.get("ref") != "main" or base_repo.get("full_name") != REPOSITORY
                or head.get("ref") != context.branch or head_repo.get("full_name") != REPOSITORY):
            raise ApiError()
        if head.get("sha") != context.sha:
            return {"status": "deferred", "reason": "branch_advanced", "pull_request_url": url}
        return {"status": "existing", "pull_request_url": url}

    def ensure(self, context):
        if not context.successful:
            return {"status": "blocked_checks"}
        query = urllib.parse.urlencode({"state": "open", "base": "main",
                                        "head": f"Astraneer:{context.branch}", "per_page": 100})
        existing = self.request("GET", f"?{query}")
        if not isinstance(existing, list) or len(existing) > 1:
            raise ApiError()
        if existing:
            return self.validate_pr(existing[0], context)
        title, change = STAGES[context.stage]
        description = (
            f"{change}\n\n"
            f"Проверен коммит `{context.sha}`:\n\n"
            "- Linux: workspace, программный Vulkan, native и браузерный WebGPU — success.\n"
            "- Windows: сборка, D3D12 WARP и native smoke — success.\n"
            "- Сервер: release Runtime, 32 UDP-клиента, 5000 существ, 10000 машин, "
            "100000 событий воды и восстановление хранилища — success.\n\n"
            f"[Результаты Acceptance]({context.run_url}). "
            f"[Состояние и ограничения этапа](https://github.com/{REPOSITORY}/blob/{context.sha}/docs/STATUS.md).\n"
        )
        created = self.request("POST", "", {"title": f"M{context.stage}: {title}", "head": context.branch,
                                            "base": "main", "body": description})
        result = self.validate_pr(created, context)
        result["created"] = True
        if result["status"] == "existing":
            result["status"] = "created"
        return result


def delivery(context, relay, token, requested, client_factory=PullRequests):
    if not requested:
        return {"status": "not_requested"}
    if not context.successful:
        return {"status": "blocked_checks"}
    if not token:
        return {"status": "unavailable", "reason": "token_missing"}
    if relay.remote_tip(context.branch) != context.sha:
        return {"status": "deferred", "reason": "branch_advanced"}
    try:
        return client_factory(token).ensure(context)
    except ApiError as error:
        result = {"status": "denied" if error.status in (401, 403) else "error"}
        if error.status is not None:
            result["http_status"] = error.status
        return result


def main(environ=None):
    env = os.environ if environ is None else environ
    try:
        context = Context.from_environment(env)
        root = Path(env.get("GITHUB_WORKSPACE", "")).resolve()
        relay = GitRelay(root)
        relay.verify_source(context)
        outcome = delivery(context, relay, env.get("GITHUB_TOKEN"), env.get("CI_REPORT_CREATE_PR") == "1")
        # Preserve an actual PR URL in the run log even if the subsequent Git
        # report push is denied. It is validated as a URL in this repository.
        if outcome.get("pull_request_url"):
            print(f"PULL_REQUEST_URL={outcome['pull_request_url']}", flush=True)
        report = make_report(context, outcome)
        commit = relay.publish(report)
        print(f"CI_REPORT_SAVED sha={context.sha} result={report['conclusion']} delivery={outcome['status']} report_commit={commit}")
        return 0
    except ScopeError as error:
        print(f"CI_REPORT_REFUSED reason={error}")
        return 2
    except GitError as error:
        print(f"CI_REPORT_FAILED operation={error.operation} exit={error.code}")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
