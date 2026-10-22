#!/usr/bin/env python3
"""LosOS Desktop booted in a VM on this Mac, with this Mac's apps as its
windows: what losos-vm.sh runs on GitHub's Macs, after it has started
macos-app-server here.

The VM is the released image under QEMU. Its root shell on the second
serial port sets things up, and the person's part is played over QMP:
keys typed and clicks made into the VM's screen, and pictures of that
screen kept. The VM reaches this Mac's server as 10.0.2.2, QEMU's address
for the machine it runs on, the way a LosOS host reaches its macOS
machine over the network.

    losos-vm.py --image RAW --version VERSION --bundle TAR --token FILE
                --server-log FILE --out DIR --work DIR [--accel hvf,tcg]
                [--only-boot]

Everything it saw lands in OUT: each step's picture of the VM's screen,
the Mac's screen beside it, the serial log, the client's log, and
steps.json with what passed.
"""

import argparse
import functools
import http.server
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import threading
import time

# The person made on first boot, from a credential homed reads
# (`home.create.*`), so no one has to type through first-boot setup.
USER = "matus"
PASSWORD = "losos"
USER_RECORD = {
    "userName": USER,
    "realName": "Matus",
    "memberOf": ["wheel"],
    "storage": "luks",
    "enforcePasswordPolicy": False,
    "secret": {"password": [PASSWORD]},
    "privileged": {
        # openssl passwd -6 -salt 0AuhuTHNXF5n0WdY losos
        "hashedPassword": [
            "$6$0AuhuTHNXF5n0WdY$rLlChxlz02BKw192B5yGaUbTD8OSWX5z/eUZLi5dcpNPt0oAssT/rMu/EKrVz4UkZW04E0mpOPRbM3fMejV3M."
        ]
    },
}

# The VM's screen, which clicks are given as fractions of.
WIDTH, HEIGHT = 1600, 1000

# Where things are on derisk's top bar.
OVERVIEW = (0.0115, 0.013)
SEARCH = (0.028, 0.013)
SKIP_TOUR = (0.4075, 0.551)
CLOSE = (0.045, 0.013)

# The client inside the VM, from the bundle, pointed at this Mac.
SERVER = "10.0.2.2:7180"
CLIENT = f"/run/aw/run.sh --server {SERVER} --token-file /run/aw/token --software"
PATH = "export PATH=/run/current-system/sw/bin"

SHIFTED = {
    "~": "grave_accent", "!": "1", "@": "2", "#": "3", "$": "4", "%": "5",
    "^": "6", "&": "7", "*": "8", "(": "9", ")": "0", "_": "minus",
    "+": "equal", "{": "bracket_left", "}": "bracket_right", "|": "backslash",
    ":": "semicolon", '"': "apostrophe", "<": "comma", ">": "dot", "?": "slash",
}
PLAIN = {
    " ": "spc", "-": "minus", "=": "equal", "[": "bracket_left",
    "]": "bracket_right", "\\": "backslash", ";": "semicolon",
    "'": "apostrophe", ",": "comma", ".": "dot", "/": "slash",
    "`": "grave_accent", "\n": "ret",
}


def log(text):
    print(f"[{time.strftime('%H:%M:%S')}] {text}", flush=True)


class Machine:
    def __init__(self, folder):
        self.folder = folder
        self.shell = None
        self.shell_buffer = b""
        self.commands = 0

    def path(self, name):
        return os.path.join(self.folder, name)

    def qmp(self, *commands):
        s = socket.socket(socket.AF_UNIX)
        s.connect(self.path("qmp.sock"))
        f = s.makefile("rw")
        f.readline()
        f.write(json.dumps({"execute": "qmp_capabilities"}) + "\n")
        f.flush()
        f.readline()
        replies = []
        for command in commands:
            f.write(json.dumps(command) + "\n")
            f.flush()
            while True:
                reply = json.loads(f.readline())
                if "event" not in reply:
                    break
            if "error" in reply:
                log(f"QMP {command['execute']}: {reply['error']}")
            replies.append(reply)
        s.close()
        return replies

    def shot(self, file):
        self.qmp({"execute": "screendump", "arguments": {"filename": file, "format": "png"}})

    def combo(self, keys):
        # Pressed and released in one batch, so a slow compositor can
        # neither repeat a key nor lose one.
        def event(key, down):
            return {"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": key}}}

        self.qmp({
            "execute": "input-send-event",
            "arguments": {
                "events": [event(k, True) for k in keys] + [event(k, False) for k in reversed(keys)]
            },
        })

    def type(self, text, every=0.6):
        for c in text:
            if c.isupper():
                keys = ["shift", c.lower()]
            elif c in SHIFTED:
                keys = ["shift", SHIFTED[c]]
            elif c in PLAIN:
                keys = [PLAIN[c]]
            else:
                keys = [c]
            self.combo(keys)
            time.sleep(every)

    def key(self, *names, every=0.6):
        for name in names:
            self.combo(name.split("+"))
            time.sleep(every)

    def click(self, x, y):
        self.qmp({
            "execute": "input-send-event",
            "arguments": {"events": [
                {"type": "abs", "data": {"axis": "x", "value": int(x * 32767)}},
                {"type": "abs", "data": {"axis": "y", "value": int(y * 32767)}},
            ]},
        })
        time.sleep(0.3)
        for down in (True, False):
            self.qmp({
                "execute": "input-send-event",
                "arguments": {"events": [{"type": "btn", "data": {"down": down, "button": "left"}}]},
            })
            time.sleep(0.3)

    def serial(self):
        with open(self.path("serial.log"), "rb") as f:
            return f.read().decode(errors="replace")

    def wait_serial(self, pattern, timeout, since=0):
        end = time.time() + timeout
        while time.time() < end:
            if re.search(pattern, self.serial()[since:]):
                return True
            time.sleep(0.2)
        return False

    def sh(self, command, timeout=60):
        """Runs a command on the root shell, and returns its exit status and
        what it printed."""
        if self.shell is None:
            self.shell = socket.socket(socket.AF_UNIX)
            self.shell.connect(self.path("shell.sock"))
            self.shell.settimeout(1)
        self.commands += 1
        mark = f"__done_{self.commands}__"
        # The echoed command has the marker before `$?`, never a number.
        self.shell.sendall(f"{command}; echo {mark} $?\n".encode())
        end = time.time() + timeout
        while time.time() < end:
            try:
                data = self.shell.recv(65536)
            except socket.timeout:
                data = b""
            self.shell_buffer += data
            text = self.shell_buffer.decode(errors="replace")
            found = re.search(re.escape(mark) + r" (\d+)", text)
            if found:
                self.shell_buffer = b""
                output = re.sub(r"\x1b\[[0-9;?]*[a-zA-Z]", "", text[: found.start()])
                return int(found.group(1)), output
        return None, self.shell_buffer.decode(errors="replace")


class Run:
    def __init__(self, machine, out, server_log):
        self.machine = machine
        self.out = out
        self.server_log = server_log
        self.steps = []
        self.started = time.time()

    def step(self, name, ok, note=""):
        self.steps.append({
            "step": name,
            "ok": ok,
            "seconds": round(time.time() - self.started),
            "note": note,
        })
        log(f"{'ok' if ok else 'FAILED'}: {name} {note}")
        with open(os.path.join(self.out, "steps.json"), "w") as f:
            json.dump(self.steps, f, indent=2)
        return ok

    def picture(self, name):
        """The VM's screen, and this Mac's beside it where there is one."""
        self.machine.shot(os.path.join(self.out, f"{name}.png"))
        if sys.platform == "darwin":
            subprocess.run(["screencapture", "-x", os.path.join(self.out, f"{name}-mac.png")])

    def wait_server(self, pattern, timeout):
        """Waits for the Mac's server to say something, such as that it
        has started capturing an app's window."""
        end = time.time() + timeout
        while time.time() < end:
            with open(self.server_log, errors="replace") as f:
                if re.search(pattern, f.read()):
                    return True
            time.sleep(1)
        return False

    def client_log(self):
        return self.machine.sh(
            f"{PATH}; journalctl --no-pager -o cat _SYSTEMD_USER_UNIT=aw.service | tail -n 400",
            timeout=60,
        )[1]


def serve(folder):
    """The bundle and the token over HTTP, for the VM to fetch from
    10.0.2.2:8000."""
    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=folder)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 8000), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()


def boot(args, machine):
    m = machine.path
    subprocess.run(
        ["qemu-img", "create", "-q", "-f", "qcow2", "-b", os.path.abspath(args.image),
         "-F", "raw", m("vm.qcow2"), "48G"],
        check=True,
    )
    with open(m("user.json"), "w") as f:
        json.dump(USER_RECORD, f)
    os.makedirs(m("tpm"), exist_ok=True)
    tpm = shutil.which("swtpm") and subprocess.run([
        "swtpm", "socket", "--tpmstate", f"dir={m('tpm')}",
        "--ctrl", f"type=unixio,path={m('tpm.sock')}", "--tpm2", "--daemon",
    ]).returncode == 0
    time.sleep(1)
    open(m("serial.log"), "w").close()
    command = [
        "qemu-system-x86_64", "-machine", "q35", "-cpu", "max",
        "-smp", str(args.cpus), "-m", str(args.memory),
    ]
    for accel in args.accel.split(","):
        command += ["-accel", accel]
    command += [
        "-drive", f"file={m('vm.qcow2')},format=qcow2,if=virtio",
        "-device", "qemu-xhci", "-device", "usb-tablet",
        "-device", f"virtio-vga,xres={WIDTH},yres={HEIGHT}", "-display", "none",
        "-fw_cfg", f"name=opt/io.systemd.credentials/home.create.{USER},file={m('user.json')}",
        "-chardev", f"file,id=s0,path={m('serial.log')}", "-serial", "chardev:s0",
        "-chardev", f"socket,id=s1,path={m('shell.sock')},server=on,wait=off",
        "-serial", "chardev:s1",
        "-qmp", f"unix:{m('qmp.sock')},server,nowait",
        "-nic", "user,model=virtio-net-pci",
    ]
    if tpm:
        command += [
            "-chardev", f"socket,id=chrtpm,path={m('tpm.sock')}",
            "-tpmdev", "emulator,id=tpm0,chardev=chrtpm", "-device", "tpm-tis,tpmdev=tpm0",
        ]
    else:
        log("no swtpm: booting without a TPM")
    log(" ".join(command))
    qemu = subprocess.Popen(command, stdout=open(m("qemu.log"), "w"), stderr=subprocess.STDOUT)
    for _ in range(100):
        if os.path.exists(m("qmp.sock")):
            break
        time.sleep(0.1)
    return qemu


def grub(args, machine, run):
    """Stops GRUB's countdown and boots the image's entry by hand, with a
    root shell on the second serial port that the rest is driven from."""
    if not run.step("GRUB menu", machine.wait_serial("executed automatically", 600)):
        return False
    machine.key("up", every=0.5)
    machine.key("c", every=1)
    v = f"losos-desktop_{args.version}"
    prompts = 1
    for line in [
        "set root=(hd0,gpt1)",
        "probe --set=losos_esp --part-uuid (hd0,gpt1)",
        f"source /EFI/Linux/{v}.grub",
        f"linux /EFI/Linux/{v}.linux $losos_cmdline console=ttyS0 systemd.debug_shell=ttyS1",
        f"initrd /EFI/Linux/{v}.initrd",
        "boot",
    ]:
        # Keys typed while GRUB is still loading the previous line's file go
        # missing (an Intel runner lost the u of "Linux" in the initrd's
        # path, and the kernel panicked without it), so each line waits for
        # GRUB's next prompt, and is checked against GRUB's echo on the
        # serial port before Enter and typed again when it does not match.
        # GRUB wraps a long line at 80 columns, hence the line breaks taken
        # out of the echo.
        end = time.time() + 300
        while machine.serial().count("grub>") < prompts and time.time() < end:
            time.sleep(0.2)
        for attempt in range(4):
            if attempt:
                log(f"GRUB echoed {echo!r} for {line!r}, typing it again")
                machine.key("ctrl+a", "ctrl+k", every=0.5)
            since = len(machine.serial())
            machine.type(line, every=0.1)
            end = time.time() + 5
            while True:
                echo = re.sub(r"\x1b\[[0-9;?]*[A-Za-z]|[\r\n]", "", machine.serial()[since:])
                if echo.endswith(line) or time.time() > end:
                    break
                time.sleep(0.2)
            if echo.endswith(line):
                break
        prompts = machine.serial().count("grub>") + 1
        machine.key("ret", every=1)
    return True


def sign_in(machine, run):
    if not run.step("booted to a login prompt", machine.wait_serial(r"login:", 3600)):
        return None
    # The VM suspends when nobody touches it, which stops the test.
    machine.sh(
        f"{PATH}; for t in sleep suspend hibernate hybrid-sleep suspend-then-hibernate; do "
        "ln -sf /dev/null /run/systemd/system/$t.target; done; systemctl daemon-reload"
    )
    end = time.time() + 1200
    while time.time() < end:
        status, out = machine.sh(f"{PATH}; systemctl is-active display-manager")
        if status == 0:
            break
        time.sleep(10)
    # The greeter draws a while after its service starts.
    time.sleep(90)
    run.picture("01-greeter")
    # The password field has the focus, under the only user.
    machine.type(PASSWORD + "\n", every=0.8)
    uid = machine.sh(f"{PATH}; id -u {USER}")[1].strip().splitlines()[-1]
    end = time.time() + 1200
    while time.time() < end:
        status, _ = machine.sh(f"test -S /run/user/{uid}/wayland-1")
        if status == 0:
            break
        time.sleep(10)
    else:
        run.picture("02-no-session")
        run.step("signed in", False)
        return None
    # derisk's desktop shows a couple of minutes after the session starts
    # under emulation.
    time.sleep(180)
    # Where QEMU's network looks like a captive portal, a "Sign in to
    # network" window opens first; it is closed, and the welcome tour
    # skipped. Neither click lands on anything when they are not there.
    machine.click(*CLOSE)
    time.sleep(20)
    machine.click(*SKIP_TOUR)
    time.sleep(20)
    run.picture("02-desktop")
    run.step("signed in", True)
    return uid


def start_client(machine, run):
    status, out = machine.sh(
        f"{PATH}; mkdir -p /run/aw && curl -sSf http://10.0.2.2:8000/bundle.tar | tar -x -C /run/aw"
        " && curl -sSf -o /run/aw/token http://10.0.2.2:8000/token && chmod -R a+rX /run/aw",
        timeout=600,
    )
    if not run.step("fetched app-windows into the VM", status == 0, out[-300:]):
        return False
    status, out = client(machine, f"--exec '{CLIENT}'", "show", unit="aw")
    if not run.step("started app-windows in the session", status == 0, out[-300:]):
        return False
    end = time.time() + 300
    while time.time() < end:
        if re.search(r"macOS apps listed", run.client_log()):
            break
        time.sleep(10)
    else:
        run.step("macOS apps listed in LosOS", False, run.client_log()[-500:])
        with open(os.path.join(run.out, "why.log"), "w") as f:
            f.write(why(machine, "aw"))
        return False
    time.sleep(30)
    return run.step("macOS apps listed in LosOS", True)


def client(machine, *arguments, unit):
    # The bundle's libxkbcommon looks for keyboard layouts and compose
    # tables where Ubuntu keeps them, and crashes building a compose table
    # when they are not there; the image has them in the store.
    return machine.sh(
        f"{PATH}; xkb=$(ls -d /nix/store/*-xkeyboard-config-*/share/X11/xkb | head -1); "
        "locale=$(ls -d /nix/store/*-libx11-*/share/X11/locale | head -1); "
        f"systemd-run --user -M {USER}@ -u {unit} --setenv=WAYLAND_DISPLAY=wayland-1 "
        "--setenv=XKB_CONFIG_ROOT=$xkb --setenv=XLOCALEDIR=$locale --setenv=RUST_BACKTRACE=1 "
        f"/run/current-system/sw/bin/sh {CLIENT} " + " ".join(arguments)
    )


def why(machine, unit):
    """What became of a client that stopped: its unit, and what the
    kernel and systemd-coredump saw."""
    return machine.sh(
        f"{PATH}; systemctl --user -M {USER}@ status {unit} --no-pager -l | tail -n 15; "
        "journalctl -b -k --no-pager | tail -n 15; coredumpctl --no-pager list | tail -n 5",
        timeout=60,
    )[1]


def window(run, app, settle=60):
    """Waits for an app's window to be captured on the Mac and drawn in
    the VM."""
    ok = run.wait_server(rf"capturing {re.escape(app)} ", 300)
    time.sleep(settle)
    return ok


def scenes(machine, run):
    ok = True

    # An app started from LosOS's own search, as a person would.
    machine.click(*SEARCH)
    time.sleep(30)
    machine.type("chess", every=1.5)
    time.sleep(30)
    run.picture("03-search-chess")
    machine.key("ret")
    found = window(run, "com.apple.Chess")
    run.picture("04-chess")
    ok &= run.step("Chess started from LosOS's search, as a LosOS window", found)

    # Typing into a macOS app from LosOS.
    client(machine, "open", "com.apple.TextEdit", unit="aw-textedit")
    found = window(run, "com.apple.TextEdit")
    run.picture("05-textedit")
    machine.type("Hello from LosOS!", every=0.8)
    time.sleep(40)
    run.picture("06-textedit-typed")
    ok &= run.step("typed into TextEdit from LosOS", found)

    client(machine, "open", "com.apple.calculator", unit="aw-calculator")
    found = window(run, "com.apple.calculator")
    machine.key("esc")
    machine.type("12*34=", every=1.0)
    time.sleep(40)
    run.picture("07-calculator")
    ok &= run.step("Calculator worked from LosOS (12*34=)", found)

    # A Mac package downloaded inside the VM, opened in macOS.
    status, out = machine.sh(
        f"{PATH}; tag=$(curl -fsSLI -o /dev/null -w '%{{url_effective}}' "
        "https://github.com/rxhanson/Rectangle/releases/latest) && v=${tag##*/v} && "
        "curl -fsSL -o /run/aw/Rectangle$v.dmg https://github.com/rxhanson/Rectangle/releases/download/v$v/Rectangle$v.dmg"
        " && chmod a+r /run/aw/Rectangle*.dmg && ls /run/aw/Rectangle*.dmg",
        timeout=600,
    )
    run.step("downloaded a Mac package inside the VM", status == 0, out[-200:])
    dmg = out.strip().splitlines()[-1] if status == 0 else ""
    client(machine, "send", dmg, unit="aw-send")
    found = run.wait_server(r"saved .*Rectangle.*opened it", 300) and window(run, "com.apple.finder")
    run.picture("08-dmg-opened")
    ok &= run.step("the dmg the VM downloaded opened in macOS, its window in LosOS", found)

    machine.click(*OVERVIEW)
    time.sleep(40)
    run.picture("09-overview")
    machine.key("esc")
    return ok


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--image", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--bundle")
    parser.add_argument("--token")
    parser.add_argument("--server-log")
    parser.add_argument("--out", required=True)
    parser.add_argument("--work", required=True, help="where the VM's disk goes")
    parser.add_argument("--accel", default="tcg")
    parser.add_argument("--cpus", type=int, default=os.cpu_count())
    parser.add_argument("--memory", type=int, default=6144)
    parser.add_argument("--only-boot", action="store_true")
    args = parser.parse_args()

    os.makedirs(args.out, exist_ok=True)
    work = args.work
    os.makedirs(work, exist_ok=True)
    machine = Machine(work)
    run = Run(machine, args.out, args.server_log)
    if args.bundle:
        files = os.path.join(work, "files")
        os.makedirs(files, exist_ok=True)
        shutil.copy(args.bundle, os.path.join(files, "bundle.tar"))
        shutil.copy(args.token, os.path.join(files, "token"))
        serve(files)

    qemu = boot(args, machine)
    ok = False
    try:
        if grub(args, machine, run) and sign_in(machine, run):
            if args.only_boot:
                ok = True
            elif start_client(machine, run):
                run.picture("02-apps-listed")
                ok = scenes(machine, run)
        else:
            run.picture("00-stuck")
    finally:
        if machine.shell is not None:
            with open(os.path.join(args.out, "client.log"), "w") as f:
                f.write(run.client_log())
        qemu.terminate()
        shutil.copy(machine.path("serial.log"), os.path.join(args.out, "serial.log"))
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
