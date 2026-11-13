"""Linux-only ownership, event readiness and offline capture isolation."""
from __future__ import annotations

import contextlib
import ctypes
import hashlib
import json
import os
from pathlib import Path
import select
import selectors
import shutil
import signal
import subprocess
import sys
import tempfile
import time


ENVIRONMENT = (
    "PATH", "LANG", "LC_ALL", "LOCALE_ARCHIVE", "FONTCONFIG_FILE", "FONTCONFIG_PATH",
    "NIX_LD", "NIX_LD_LIBRARY_PATH", "LD_LIBRARY_PATH", "SHOWCASE_FONT", "PYTHONPATH",
    "LIBGL_DRIVERS_PATH", "__EGL_VENDOR_LIBRARY_FILENAMES", "__GLX_VENDOR_LIBRARY_NAME",
    "GALLIUM_DRIVER",
)


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def native(command: str | Path) -> Path:
    found = Path(shutil.which(str(command)) or command).absolute()
    candidate = found.parent.resolve(strict=True) / found.name
    with candidate.open("rb") as stream:
        if stream.read(4) != b"\x7fELF":
            raise RuntimeError(f"Expected native executable, not a profile wrapper: {candidate}")
    return candidate


def private_environment(root: Path, inherited: dict[str, str]) -> dict[str, str]:
    env = {key: inherited[key] for key in ENVIRONMENT if key in inherited}
    for key, leaf in (
        ("HOME", "h"), ("XDG_CONFIG_HOME", "c"), ("XDG_CACHE_HOME", "k"),
        ("XDG_DATA_HOME", "d"), ("XDG_STATE_HOME", "s"), ("XDG_RUNTIME_DIR", "r"),
    ):
        directory = root / leaf
        directory.mkdir(mode=0o700)
        env[key] = str(directory)
    env.update(
        TERM="xterm-kitty", LIBGL_ALWAYS_SOFTWARE="1", GALLIUM_DRIVER="llvmpipe", PYTHONDONTWRITEBYTECODE="1",
        DBUS_SESSION_BUS_ADDRESS=f"unix:path={root}/no-bus", GTK_USE_PORTAL="0",
        GIO_USE_VFS="local", NO_AT_BRIDGE="1", GIT_CONFIG_NOSYSTEM="1",
        GIT_CONFIG_GLOBAL="/dev/null", GIT_TERMINAL_PROMPT="0",
    )
    if drivers := env.get("LIBGL_DRIVERS_PATH"):
        mesa_lib = Path(drivers).parent
        if not (mesa_lib / "libGLX_mesa.so.0").is_file():
            raise RuntimeError(f"Pinned Mesa GLX vendor library is absent: {mesa_lib}")
        libraries = [str(mesa_lib), *filter(None, env.get("LD_LIBRARY_PATH", "").split(os.pathsep))]
        env["LD_LIBRARY_PATH"] = os.pathsep.join(dict.fromkeys(libraries))
    return env


def process_identity(pid: int) -> dict | None:
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return {"pid": pid, "birth": fields[19], "state": fields[0]}
    except (OSError, IndexError):
        return None


class OwnedProcesses:
    def __init__(self, env: dict[str, str], evidence: Path):
        self.env, self.evidence = env, evidence
        self.children: list[tuple[str, subprocess.Popen]] = []
        self.births: dict[int, str] = {}
        libc = ctypes.CDLL(None, use_errno=True)
        if libc.prctl(36, 1, 0, 0, 0):
            raise OSError(ctypes.get_errno(), "PR_SET_CHILD_SUBREAPER")

    def discover(self) -> None:
        pending, seen = [os.getpid()], set()
        pending.extend(pid for pid, birth in self.births.items()
                       if (identity := process_identity(pid)) and identity["birth"] == birth)
        while pending:
            pid = pending.pop()
            if pid in seen:
                continue
            seen.add(pid)
            if pid != os.getpid() and (identity := process_identity(pid)):
                self.births[pid] = identity["birth"]
            for children in Path(f"/proc/{pid}/task").glob("*/children"):
                with contextlib.suppress(OSError):
                    pending.extend(map(int, children.read_text().split()))

    def start(self, label: str, argv: list, **kwargs) -> subprocess.Popen:
        with (self.evidence / f"{label}.log").open("wb") as log:
            child = subprocess.Popen(list(map(str, argv)), env=self.env, start_new_session=True,
                                     stdout=log, stderr=subprocess.STDOUT, **kwargs)
        self.children.append((label, child))
        self.discover()
        return child

    def command(self, argv: list, *, timeout: float = 30, **kwargs) -> subprocess.CompletedProcess:
        result = subprocess.run(list(map(str, argv)), env=self.env, capture_output=True,
                                timeout=timeout, check=False, **kwargs)
        if result.returncode:
            raise RuntimeError(f"Owned command {argv[0]} exited {result.returncode}: "
                               + result.stderr[:16000].decode(errors="replace"))
        return result

    def close(self) -> dict:
        self.discover()
        for sig, duration in ((signal.SIGTERM, 5), (signal.SIGKILL, 5)):
            deadline = time.monotonic() + duration
            with selectors.DefaultSelector() as events:
                for pid, birth in list(self.births.items()):
                    try:
                        fd = os.pidfd_open(pid)
                    except ProcessLookupError:
                        continue
                    identity = process_identity(pid)
                    if identity and identity["birth"] == birth:
                        with contextlib.suppress(ProcessLookupError):
                            signal.pidfd_send_signal(fd, sig)
                        events.register(fd, selectors.EVENT_READ)
                    else:
                        os.close(fd)
                while events.get_map():
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        break
                    for key, _ in events.select(remaining):
                        events.unregister(key.fd)
                        os.close(key.fd)
                for fd in list(events.get_map()):
                    events.unregister(fd)
                    os.close(fd)
            for _, child in self.children:
                child.poll()
            while True:
                try:
                    pid, _ = os.waitpid(-1, os.WNOHANG)
                except ChildProcessError:
                    break
                if pid == 0:
                    break
            self.discover()
            if not self.survivors():
                break
        receipt = {
            "owned_births": {str(pid): birth for pid, birth in self.births.items()},
            "survivors": self.survivors(), "signals": "pidfd with birth revalidation",
            "children": [{"label": label, "pid": child.pid, "returncode": child.poll()}
                         for label, child in self.children],
        }
        write_json(self.evidence / "cleanup.json", receipt)
        if receipt["survivors"]:
            raise RuntimeError("Owned processes survived cleanup; see cleanup.json")
        return receipt

    def survivors(self) -> list[dict]:
        return [identity for pid, birth in self.births.items()
                if (identity := process_identity(pid)) and identity["birth"] == birth]


class Changes:
    """Block on filesystem changes instead of sleep-based readiness probes."""

    def __init__(self, paths: list[Path]):
        self.libc = ctypes.CDLL(None, use_errno=True)
        self.fd = self.libc.inotify_init1(os.O_CLOEXEC | os.O_NONBLOCK)
        if self.fd < 0:
            raise OSError(ctypes.get_errno(), "inotify_init1")
        for path in set(paths):
            if self.libc.inotify_add_watch(self.fd, os.fsencode(path), 0x00000D8E) < 0:
                self.close()
                raise OSError(ctypes.get_errno(), f"inotify_add_watch: {path}")

    def wait(self, timeout: float) -> bool:
        if not select.select([self.fd], [], [], max(timeout, 0))[0]:
            return False
        with contextlib.suppress(BlockingIOError):
            while os.read(self.fd, 65536):
                pass
        return True

    def until(self, predicate, label: str, timeout: float = 60):
        deadline = time.monotonic() + timeout
        while True:
            if value := predicate():
                return value
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not self.wait(remaining):
                raise TimeoutError(label)

    def close(self) -> None:
        os.close(self.fd)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


def wait_path(path: Path, timeout: float = 30) -> None:
    with Changes([path.parent]) as changes:
        changes.until(path.exists, f"Waiting for owned path: {path}", timeout)


def start_display(owned: OwnedProcesses, tools: dict[str, Path]) -> str:
    reader, writer = os.pipe()
    try:
        owned.start("xvfb", [tools["Xvfb"], "-displayfd", writer, "-noreset", "-nolisten", "tcp",
                             "-screen", "0", "2400x1500x24", "-ac"], pass_fds=(writer,))
    finally:
        os.close(writer)
    try:
        if not select.select([reader], [], [], 20)[0]:
            raise TimeoutError("Owned Xvfb display readiness")
        display = ":" + os.read(reader, 64).decode().strip()
        if display == ":":
            raise RuntimeError("Xvfb exited before reporting a display")
        return display
    finally:
        os.close(reader)


def enter_namespace(root: Path, mounts: list[tuple[Path, bool]], argv: list[str]) -> None:
    uid = os.getuid()
    if Path("/proc/self/uid_map").read_text().split() != [str(uid), str(uid), "1"]:
        raise RuntimeError("Capture requires current-user namespace, never root mapping")
    mount = native("mount")
    subprocess.run([native("ip"), "link", "set", "lo", "up"], check=True)
    subprocess.run([mount, "--make-rprivate", "/"], check=True)

    def bind(source: Path, writable: bool = False) -> None:
        destination = root / source.relative_to("/")
        if source.is_symlink():
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.symlink_to(os.readlink(source))
            return
        if source.is_dir():
            destination.mkdir(parents=True, exist_ok=True)
        else:
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.touch()
        subprocess.run([mount, "--rbind", source, destination], check=True)
        if not writable:
            subprocess.run([mount, "-o", "remount,bind,ro", destination], check=True)

    for path in ("/nix", "/etc", "/proc", "/bin", "/usr", "/lib", "/lib64",
                 "/run/current-system"):
        if (source := Path(path)).exists():
            bind(source)
    for source, writable in mounts:
        bind(source, writable)
    dev = root / "dev"
    dev.mkdir()
    subprocess.run([mount, "-t", "tmpfs", "-o", "mode=755", "tmpfs", dev], check=True)
    for name in ("null", "zero", "random", "urandom", "tty"):
        bind(Path("/dev") / name, True)
    (dev / "pts").mkdir()
    subprocess.run([mount, "-t", "devpts", "-o", "newinstance,ptmxmode=0666,mode=0620", "devpts", dev / "pts"], check=True)
    (dev / "ptmx").symlink_to("pts/ptmx")
    (dev / "fd").symlink_to("/proc/self/fd")
    (dev / "shm").mkdir(mode=0o700)
    (root / "tmp/.X11-unix").mkdir(parents=True, mode=0o700)
    python = native(sys.executable)
    os.chroot(root)
    os.chdir("/tmp")
    os.execve(python, [str(python), *argv], {key: os.environ[key] for key in ENVIRONMENT if key in os.environ})


def supervise_namespace(evidence: Path, script: Path, arguments: list[str]) -> None:
    root = Path(tempfile.mkdtemp(prefix="bkshot-ns-"))
    env = {key: os.environ[key] for key in ENVIRONMENT if key in os.environ}
    env["PYTHONPATH"] = os.pathsep.join(
        path for path in sys.path if path and Path(path).is_absolute()
        and Path(path).resolve().is_relative_to("/nix/store")
    )
    namespace_evidence = evidence / "namespace"
    namespace_evidence.mkdir(mode=0o700)
    owned = OwnedProcesses(env, namespace_evidence)
    try:
        command = [native("unshare"), "--user", "--map-current-user", "--keep-caps", "--mount",
                   "--net", "--fork", "--kill-child", "--", native(sys.executable), script,
                   "--namespace-root", root, *arguments]
        child = owned.start("controller", command, cwd=evidence)
        status = child.wait(timeout=300)
        if status:
            log = (namespace_evidence / "controller.log").read_text(errors="replace")
            raise RuntimeError(f"Isolated showcase exited {status}:\n{log[-16000:]}")
    finally:
        try:
            owned.close()
        finally:
            shutil.rmtree(root)
