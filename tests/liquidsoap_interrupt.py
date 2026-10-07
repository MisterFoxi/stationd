#!/usr/bin/env python3
"""Run an isolated Liquidsoap hard cut and check the returned audio.

Usage: python3 tests/liquidsoap_interrupt.py
Requires Liquidsoap in PATH. Uses an isolated script and temporary control socket.
"""
import array
import math
import json
import re
import pathlib
import subprocess
import socket
import time
import tempfile
import wave

ROOT = pathlib.Path(__file__).resolve().parents[1]
RATE = 44100


def tone(path, frequency, seconds):
    pcm = array.array("h", (
        int(12000 * math.sin(2 * math.pi * frequency * n / RATE))
        for n in range(int(seconds * RATE)) for _ in range(2)
    ))
    with wave.open(str(path), "wb") as audio:
        audio.setnchannels(2)
        audio.setsampwidth(2)
        audio.setframerate(RATE)
        audio.writeframes(pcm.tobytes())


def power(samples, frequency):
    real = sum(x * math.cos(2 * math.pi * frequency * n / RATE)
               for n, x in enumerate(samples))
    imag = sum(x * math.sin(2 * math.pi * frequency * n / RATE)
               for n, x in enumerate(samples))
    return (real * real + imag * imag) / len(samples) ** 2


def check(crossfade, command=None, dynamic=False, during_transition=False, cut_at=None, socket_control=True, retry_delay=2.):
    text = (ROOT / "src/ls_script.rs").read_text()
    command = command or text.split("def stationd.cmd_interrupt(uri) =", 1)[1].split("\nend\n", 1)[0]
    # The production command is taken from the generator, rather than copied.
    with tempfile.TemporaryDirectory(prefix="stationd-cut-") as directory:
        work = pathlib.Path(directory)
        cut_at = cut_at if cut_at is not None else (10 if during_transition else 5)
        return_frequency = 1320 if during_transition else 880
        tone(work / "old.wav", 440, 12 if during_transition else 30)
        tone(work / "next.wav", 880, 30)
        tone(work / "last.wav", 1320, 30)
        tone(work / "insert.wav", 1760, 2)
        # Decode the generated Rust string, including its line continuations,
        # so the actual transition and buffer purge are tested together.
        encoded = '"def stationd.transition(a, b) =' + text.split(
            '"def stationd.transition(a, b) =', 1)[1].split('",\n            fade =', 1)[0] + '"'
        encoded = re.sub(r"\\\n\s*", "", encoded)
        cross = json.loads(encoded).replace("{fade}", "2.").replace("{dur}", "3.") if crossfade else "def stationd.make_chain(pull_raw) = pull_raw end\n"
        raw = "def stationd.make_raw() =" + text.split(
            "def stationd.make_raw() =", 1)[1].split("\nlet stationd.raw_pull", 1)[0]
        raw = raw.replace("{retry}", str(retry_delay)).replace("{rto}", "5.")
        source = f"""
let stationd.urgent = ref(true)
let stationd.next_not_before = ref(0.)
let stationd.remaining = ref(fun () -> -1.)
let stationd.pull_generation = ref(0)
let stationd.relay_off = ref(fun () -> ())
let stationd.air_elapsed = ref(fun () -> 0.)
def stationd.report(_, _) = () end
let count = ref(0)
def stationd.next() =
  remaining = stationd.remaining()
  if not stationd.urgent() and remaining() > 7. then
    null
  elsif {str(dynamic).lower()} and count() < 3 then
    stationd.urgent := false
    filename = if count() == 0 then "old.wav" elsif count() == 1 then "next.wav" else "last.wav" end
    count := count() + 1
    request.create("{work}/" ^ filename)
  else
    null
  end
end
{raw}
let stationd.raw_pull = ref(stationd.make_raw())
"""
        if not dynamic:
            files = ["old.wav", "next.wav"] + (["last.wav"] if during_transition else [])
            requests = ", ".join(f'request.create("{work}/{name}")' for name in files)
            source += f'initial = stationd.raw_pull()\ninitial.set_queue([{requests}])\n'
        chain_start = text.index("let stationd.pull_chain = ref(")
        cross += text[chain_start:text.index('"##,', chain_start)]
        flush_body = text.split("def stationd.cmd_flush(_) =", 1)[1].split("\nend\n", 1)[0]
        cross += "def stationd.cmd_flush(_) =" + flush_body + "\nend\n"
        flush = 'ignore(stationd.cmd_flush(""))' if dynamic else ""
        trigger = "" if socket_control else f"""
  if played() >= {cut_at}. and not cut() then
    cut := true
    {flush}
    ignore(stationd.cmd_interrupt("{work}/insert.wav"))
  end
"""
        control = f"""
settings.server.socket := true
settings.server.socket.path := "{work}/control.sock"
""" if socket_control else ""
        commands = f"""
server.register(namespace="test", description="Audio position", "time", fun (_) -> "#{{played()}}")
def cut_now(_) =
  position = played()
  {flush}
  ignore(stationd.cmd_interrupt("{work}/insert.wav"))
  "#{{position}}"
end
server.register(namespace="test", description="Cut", "cut", cut_now)
""" if socket_control else ""
        script = f"""
settings.log.stdout := true
settings.log.level := 2
{control}
stationd = ()
let stationd.no_cross = ref(false)
{source}
{cross}
interrupt = request.queue()
def stationd.cmd_interrupt(uri) =
{command}
end
radio = fallback(track_sensitive=false, [interrupt, pull, blank()])
let cut = ref(false)
let played = ref(0.)
let done = ref(false)
source.methods(radio).on_frame(synchronous=true, fun () -> begin
  played := played() + 0.02
{trigger}

  if played() >= {cut_at + 9}. and not done() then
    done := true
    shutdown()
  end
end)
{commands}
clock.assign_new(sync="cpu", [radio])
output.file(%wav(stereo=true), "{work}/out.wav", fallible=true, radio)
"""
        (work / "test.liq").write_text(script)
        process = subprocess.Popen(["liquidsoap", str(work / "test.liq")],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        def control_command(command):
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
                client.settimeout(2)
                client.connect(str(work / "control.sock"))
                client.sendall((command + "\n").encode())
                response = b""
                while not response.replace(b"\r", b"").endswith(b"\nEND\n"):
                    chunk = client.recv(4096)
                    if not chunk:
                        raise AssertionError("control socket closed")
                    response += chunk
                return float(response.decode().splitlines()[0])
        try:
            if socket_control:
                deadline = time.monotonic() + 40
                while time.monotonic() < deadline:
                    if process.poll() is not None:
                        break
                    try:
                        position = control_command("test.time")
                    except (FileNotFoundError, ConnectionRefusedError):
                        time.sleep(0.01)
                        continue
                    if position >= cut_at:
                        cut_at = control_command("test.cut")
                        break
                    time.sleep(0.01)
                else:
                    raise AssertionError("cut deadline expired")
            stdout, stderr = process.communicate(timeout=45)
            result = subprocess.CompletedProcess(process.args, process.returncode, stdout, stderr)
        except BaseException:
            process.kill()
            stdout, stderr = process.communicate()
            raise AssertionError(stdout + stderr)
        if result.returncode:
            raise AssertionError(result.stdout + result.stderr)
        with wave.open(str(work / "out.wav"), "rb") as audio:
            assert audio.getframerate() == RATE
            pcm = array.array("h", audio.readframes(audio.getnframes()))[::2]
        assert len(pcm) >= (cut_at + 7) * RATE, result.stdout + result.stderr
        # Before the cut, the original song must have been audible.
        before = pcm[3 * RATE:4 * RATE]
        assert power(before, 440) > power(before, 880) * 100
        assert power(pcm[int(cut_at * RATE):int((cut_at + 3) * RATE)], 1760) > 100_000, "insert was not audible"
        # After the insert, every return window must contain the next song,
        # with no remaining fragment of the interrupted song.
        # Start immediately after the insert, using short windows so a small
        # frozen crossfade tail cannot hide inside a whole second of new audio.
        for step in range(22, 62, 2):
            second = cut_at + step / 10
            start = int(second * RATE)
            returned = pcm[start:start + RATE // 10]
            interrupted = max(power(returned, 440),
                              power(returned, 880) if during_transition else 0)
            assert power(returned, return_frequency) > interrupted * 100, (
                crossfade, dynamic, during_transition, second,
                interrupted, power(returned, return_frequency), result.stdout, result.stderr
            )
        print(f"PASS hard cut: crossfade={crossfade}, dynamic={dynamic}, transition={during_transition}; no interrupted audio on return", flush=True)


if __name__ == "__main__":
    check(True)
    check(False)
    check(True, dynamic=True)
    check(False, dynamic=True)
    check(True, during_transition=True)
    check(True, dynamic=True, during_transition=True)
    check(True, during_transition=True, cut_at=12)
