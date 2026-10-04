#!/usr/bin/env python3
"""Run an isolated Liquidsoap hard cut and check the returned audio.

Usage: python3 tests/liquidsoap_interrupt.py
Requires Liquidsoap in PATH. Uses no station services or control sockets.
"""
import array
import math
import pathlib
import subprocess
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


def check(crossfade, command=None):
    text = (ROOT / "src/ls_script.rs").read_text()
    command = command or text.split("def stationd.cmd_interrupt(uri) =", 1)[1].split("\nend\n", 1)[0]
    # The production command is taken from the generator, rather than copied.
    with tempfile.TemporaryDirectory(prefix="stationd-cut-") as directory:
        work = pathlib.Path(directory)
        tone(work / "old.wav", 440, 30)
        tone(work / "next.wav", 880, 30)
        tone(work / "insert.wav", 1760, 2)
        cross = """
def transition(a, b) =
  if stationd.no_cross() then
    stationd.no_cross := false
    b.source
  else
    cross.simple(a.source, b.source, fade_in=2., fade_out=2.)
  end
end
pull = cross(duration=3., transition, pull)
""" if crossfade else ""
        script = f"""
settings.log.stdout := true
settings.log.level := 3
stationd = ()
let stationd.no_cross = ref(false)
pull = request.queue()
pull.push(request.create("{work}/old.wav"))
pull.push(request.create("{work}/next.wav"))
pull_raw = pull
{cross}
interrupt = request.queue()
def stationd.cmd_interrupt(uri) =
{command}
end
radio = fallback(track_sensitive=false, [interrupt, pull])
let cut = ref(false)
let played = ref(0.)
let done = ref(false)
source.methods(radio).on_frame(synchronous=true, fun () -> begin
  played := played() + 0.02
  if played() >= 5. and not cut() then
    cut := true
    ignore(stationd.cmd_interrupt("{work}/insert.wav"))
  end
  if played() >= 14. and not done() then
    done := true
    shutdown()
  end
end)
clock.assign_new(sync="cpu", [radio])
output.file(%wav(stereo=true), "{work}/out.wav", fallible=true, radio)
"""
        (work / "test.liq").write_text(script)
        try:
            result = subprocess.run(["liquidsoap", str(work / "test.liq")],
                                    capture_output=True, text=True, timeout=45)
        except subprocess.TimeoutExpired as error:
            raise AssertionError((error.stdout or b"").decode() + (error.stderr or b"").decode()) from error
        if result.returncode:
            raise AssertionError(result.stdout + result.stderr)
        with wave.open(str(work / "out.wav"), "rb") as audio:
            assert audio.getframerate() == RATE
            pcm = array.array("h", audio.readframes(audio.getnframes()))[::2]
        assert len(pcm) >= 12 * RATE, result.stdout + result.stderr
        # Before the cut, the original song must have been audible.
        before = pcm[3 * RATE:4 * RATE]
        assert power(before, 440) > power(before, 880) * 100
        assert power(pcm[5 * RATE:8 * RATE], 1760) > 100_000, "insert was not audible"
        # After the insert, every return window must contain the next song,
        # with no remaining fragment of the interrupted song.
        for second in range(8, 12):
            returned = pcm[second * RATE:(second + 1) * RATE]
            assert power(returned, 880) > power(returned, 440) * 100, (
                crossfade, second, power(returned, 440), power(returned, 880)
            )
        print(f"PASS hard cut: crossfade={crossfade}; no interrupted audio on return", flush=True)


if __name__ == "__main__":
    check(True)
    check(False)