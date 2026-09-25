//! Liquidsoap script generator (pure: config in, `.liq` text out).
//!
//! In this architecture Liquidsoap decides NOTHING: the grid, the playlists,
//! the overrides and the broadcast gate all live in stationd. The generated
//! script is only an air chain plus the bridge back to stationd:
//!
//! ```text
//! pull   = request.dynamic(POST /ls/v1/next)   # stationd resolves every track,
//!                                              # asked only near the end of the
//!                                              # current one (real air time)
//! pull   = cross(pull)                         # optional crossfade
//! radio  = fallback(track_sensitive=false, [
//!            relay          (a `remote` playlist: input.http, once the pull
//!                            has no track left — soft entry),
//!            pull,
//!            halted noise   (while stationd reports paused/stopped),
//!            blank          (until stationd answered once — start-up),
//!            safety fallback (nothing to air / stationd unreachable) ])
//! radio  → normalize/compress (optional) → custom include → icecast outputs
//! on_track(pull | halted noise | fallback) → POST /ls/v1/track  # what REALLY airs
//! ```
//!
//! Halted ≠ fallback: when the station is paused/stopped stationd answers
//! `halted`, and the background noise fills the air — never the safety file.
//! The current track is never cut: the pull source stays first while it still
//! has a track; the noise only takes over at its end.
//!
//! Track starts are observed on each source BEFORE the air-chain operators
//! (the pull, the noise, the fallback): a track that enters through a
//! crossfade does not raise `on_track` downstream of `cross` (seen on a real
//! Liquidsoap), while the pull source always does — when it starts being
//! read, i.e. at the start of the transition.
//!
//! The next track is asked for LATE: `request.dynamic` keeps one request
//! ahead, which would make stationd choose it a whole track before it airs
//! (a day part, an `every`, the history, all one track off). The pull
//! function therefore answers "nothing yet" while the current track has more
//! than [`pull_lead_s`] left, without calling stationd (`request.dynamic`
//! re-polls every retry delay): the choice happens a few seconds before the
//! track airs. At start-up, after a track ended, or when the remaining time is
//! unknown, it asks at once. A skip must not wait: it raises `urgent` and
//! fetches the next track synchronously before skipping (no gap filled by the
//! safety file).
//!
//! A `remote` playlist is relayed, not played as a file: `/next` answers
//! `relay` + the URL, the script (re)starts `input.http` on it and queues
//! nothing. The relay takes the air once the current track is over (soft,
//! like a day-part change) and keeps it while stationd keeps answering
//! `relay` — the pull, trackless, re-asks every retry delay: that polling IS
//! the watch on the grid. A `file` answer queues the track: the relay yields
//! as soon as it is ready, and stops when it starts; `halted` / `none` stop the
//! relay at once (noise / safety are always ready). A hard insert plays over
//! the relay, which then resumes live.
//!
//! Every user-provided string is emitted through [`liq_string`], which also
//! neutralises Liquidsoap's `#{…}` interpolation.

use std::path::Path;

use crate::config::{CrossfadeMode, IcecastOutput, LiquidsoapConfig, OutputFormat};

/// Header name Liquidsoap sends the shared token in (lower-case: HTTP header
/// names are case-insensitive, axum normalises them).
pub const TOKEN_HEADER: &str = "x-stationd-token";

/// Retry delay of the pull source when stationd returns nothing (halted,
/// fallback): also the polling period while halted, i.e. the resume latency.
const PULL_RETRY_S: f64 = 2.0;
/// HTTP timeout of a bridge call, seconds.
const HTTP_TIMEOUT_S: f64 = 5.0;
/// Timeout for preparing one request (file resolution), seconds.
const REQUEST_TIMEOUT_S: f64 = 20.0;
/// Margin, beyond the crossfade and one retry delay, for stationd to answer
/// and Liquidsoap to prepare the next track before it is needed.
const PULL_LEAD_MARGIN_S: f64 = 2.0;

/// How long before the end of the current track (seconds of it left) the next
/// one is asked for: the crossfade overlap (the next track must be ready when
/// the mix starts) + one pull retry delay (the pull re-polls at that period)
/// + [`PULL_LEAD_MARGIN_S`]. 7 s with the default crossfade.
pub fn pull_lead_s(ls: &LiquidsoapConfig) -> f64 {
    let overlap = match ls.crossfade.mode {
        CrossfadeMode::Simple => ls.crossfade.duration,
        CrossfadeMode::None => 0.0,
    };
    overlap + PULL_RETRY_S + PULL_LEAD_MARGIN_S
}

/// A Liquidsoap string literal. Escapes `\` and `"`, turns control chars into
/// escapes, and splits every `#{` so Liquidsoap never interpolates it:
/// `a#{b` → `("a#" ^ "{b")`.
pub fn liq_string(s: &str) -> String {
    let mut esc = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\\' => esc.push_str("\\\\"),
            '"' => esc.push_str("\\\""),
            '\n' => esc.push_str("\\n"),
            '\r' => esc.push_str("\\r"),
            '\t' => esc.push_str("\\t"),
            c => esc.push(c),
        }
    }
    if esc.contains("#{") {
        format!("(\"{}\")", esc.replace("#{", "#\" ^ \"{"))
    } else {
        format!("\"{esc}\"")
    }
}

/// A float literal Liquidsoap accepts (always with a decimal point).
fn liq_float(x: f64) -> String {
    let s = format!("{x}");
    if s.contains('.') || s.contains('e') {
        s
    } else {
        format!("{s}.")
    }
}

/// Liquidsoap does not share stationd's working directory: every path handed
/// to it is made absolute (against stationd's CWD, like the rest of the
/// config). Falls back to the path as given if the CWD is unreadable.
fn absolute(p: &Path) -> std::path::PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

fn liq_path(p: &Path) -> String {
    liq_string(&p.to_string_lossy())
}

/// Render the whole script. `station_name` is the default Icecast stream name.
pub fn render(ls: &LiquidsoapConfig, station_name: &str) -> String {
    let mut o = String::new();
    let api_url = format!("http://{}/ls/v1", ls.http_bind);

    o.push_str(&format!(
        "# ─────────────────────────────────────────────────────────────────────\n\
         # GENERATED BY stationd {} — DO NOT EDIT.\n\
         # Rewritten at stationd start-up from [liquidsoap] in stationd.toml;\n\
         # restart Liquidsoap to pick up a change. Customise through\n\
         # [liquidsoap] custom_include (it sees and may reassign `radio`).\n\
         # Station: {}\n\
         # ─────────────────────────────────────────────────────────────────────\n\n",
        env!("CARGO_PKG_VERSION"),
        station_name.replace('\n', " ")
    ));

    o.push_str(&format!(
        "log.stdout := true\n\
         log.file := false\n\
         log.level := {}\n\
         settings.encoder.metadata.export := [\"artist\", \"title\", \"album\", \"song\"]\n\n\
         # Control socket (stationd → Liquidsoap): pause / resume / skip.\n\
         # Group-writable: the stationd user must be in Liquidsoap's group.\n\
         settings.server.socket := true\n\
         settings.server.socket.path := {socket}\n\
         settings.server.socket.permissions := 0o660\n\n",
        ls.log_level,
        socket = liq_path(&absolute(&ls.control_socket)),
    ));

    // ── bridge ────────────────────────────────────────────────────────────
    o.push_str(&format!(
        r##"# ─── stationd bridge ──────────────────────────────────────────────────
stationd = ()
let stationd.api_url = {api_url}
let stationd.api_token = {token}
# True while stationd reports the station paused/stopped (at a track boundary).
let stationd.halted = ref(false)
# True while paused through the control socket: immediate, the current track
# is frozen (not read) and resumes where it stopped.
let stationd.paused = ref(false)
# True until stationd answered once (start-up: silence, not the fallback file).
let stationd.loading = ref(true)
# Last reply kind, to log transitions only (the pull polls while idle).
let stationd.last_kind = ref("")
# After an empty reply (halted, nothing to air, stationd unreachable), no new
# call before this instant: the air chain may re-poll the pull many times per
# second around a track end (seen on a real Liquidsoap). Bounds the polling to
# one call per retry delay — also the resume latency.
let stationd.next_not_before = ref(0.)
# The next track is asked for only when the current one has at most this many
# seconds left (see `stationd.next`): chosen at its real air time.
let stationd.lead = {lead}
# Raised by a skip: ask stationd now, whatever the current track has left.
let stationd.urgent = ref(true)
# Seconds left in the current pull track (-1. = unknown / none). Set once the
# pull source exists (it is defined after the function that reads this).
let stationd.remaining = ref(fun () -> -1.)
# Relay of a `remote` playlist (input.http): started / stopped from the pull
# replies. The two functions are wired once the relay source exists.
let stationd.relay_url = ref("")
let stationd.relaying = ref(false)
let stationd.relay_on = ref(fun (_) -> ())
let stationd.relay_off = ref(fun () -> ())

def stationd.post(endpoint, payload) =
  try
    r = http.post(
      stationd.api_url ^ "/" ^ endpoint,
      headers=[
        ("Content-Type", "application/json"),
        ("User-Agent", "Liquidsoap stationd"),
        ("X-Stationd-Token", stationd.api_token)
      ],
      timeout={http_timeout},
      data=payload
    )
    if r.status_code == 200 then
      "#{{r}}"
    else
      log.important(label="stationd", "#{{endpoint}}: HTTP #{{r.status_code}}")
      null
    end
  catch err do
    log.severe(label="stationd", "#{{endpoint}}: #{{error.kind(err)}}: #{{error.message(err)}}")
    null
  end
end

def stationd.transition_log(kind, detail) =
  if stationd.last_kind() != kind then
    log.important(label="stationd", "next: #{{kind}} #{{detail}}")
    stationd.last_kind := kind
  end
end

# Pull: stationd resolves the next track (grid, overrides, broadcast gate).
def stationd.fetch_next() =
  resp = stationd.post("next", "{{}}")
  stationd.loading := false
  if null.defined(resp) then
    try
      let json.parse ({{kind, uri, state, reason}} : {{
        kind: string, uri: string, state: string, reason: string
      }}) = null.get(resp)
      if kind == "file" then
        # A running relay yields once this track is ready, stops when it starts.
        stationd.halted := false
        stationd.transition_log(kind, "")
        request.create(uri)
      elsif kind == "relay" then
        stationd.halted := false
        stationd.transition_log(kind, uri)
        relay_on = stationd.relay_on()
        relay_on(uri)
        null
      elsif kind == "halted" then
        relay_off = stationd.relay_off()
        relay_off()
        stationd.halted := true
        stationd.transition_log(kind, "(#{{state}}): halted noise at the end of the current track")
        null
      else
        relay_off = stationd.relay_off()
        relay_off()
        stationd.halted := false
        stationd.transition_log(kind, "(#{{reason}}): safety fallback")
        null
      end
    catch err do
      log.severe(label="stationd", "next: bad reply: #{{error.kind(err)}}: #{{error.message(err)}}")
      null
    end
  else
    null
  end
end

def stationd.next() =
  remaining = stationd.remaining()
  left = remaining()
  if not stationd.urgent() and left > stationd.lead then
    # Too early: the current track still has more than `lead` s to go.
    # Nothing asked; request.dynamic polls again after its retry delay.
    null
  elsif time() < stationd.next_not_before() then
    null
  else
    stationd.urgent := false
    r = stationd.fetch_next()
    if not null.defined(r) then
      stationd.next_not_before := time() + {retry}
    end
    r
  end
end

# Report what REALLY starts airing: one of our tracks (rid) or one of
# Liquidsoap's own sources (kind = halted | fallback).
def stationd.report(rid, kind) =
  j = json()
  j.add("rid", rid)
  j.add("kind", kind)
  ignore(stationd.post("track", json.stringify(compact=true, j)))
end

"##,
        api_url = liq_string(&api_url),
        token = liq_string(&ls.api_token),
        http_timeout = liq_float(HTTP_TIMEOUT_S),
        retry = liq_float(PULL_RETRY_S),
        lead = liq_float(pull_lead_s(ls)),
    ));

    // ── sources ───────────────────────────────────────────────────────────
    o.push_str(&format!(
        "# ─── air chain ─────────────────────────────────────────────────────────\n\
         pull = request.dynamic(id=\"stationd_pull\", retry_delay={retry}, timeout={rto}, stationd.next)\n\
         # One of our tracks starts: a relay it replaces is over.\n\
         def stationd.pull_started(m) =\n  \
           relay_off = stationd.relay_off()\n  \
           relay_off()\n  \
           stationd.report(m[\"stationd_rid\"], \"\")\n\
         end\n\
         source.methods(pull).on_track(synchronous=false, stationd.pull_started)\n\
         # Skip target: the track source itself, before the crossfade.\n\
         pull_raw = pull\n\
         stationd.remaining := fun () -> pull_raw.remaining()\n",
        retry = liq_float(PULL_RETRY_S),
        rto = liq_float(REQUEST_TIMEOUT_S),
    ));

    match ls.crossfade.mode {
        CrossfadeMode::None => o.push_str("# crossfade: none (hard cut)\n"),
        CrossfadeMode::Simple => o.push_str(&format!(
            "def stationd.transition(a, b) =\n  \
               cross.simple(a.source, b.source, fade_in={fade}, fade_out={fade})\n\
             end\n\
             pull = cross(id=\"stationd_cross\", duration={dur}, stationd.transition, pull)\n",
            fade = liq_float(ls.crossfade.fade),
            dur = liq_float(ls.crossfade.duration),
        )),
    }

    o.push_str(&format!(
        "\n# Liquidsoap's own sources. Plain `single` on a local file (no annotate:)\n\
         # stays infallible.\n\
         halted_noise = single(id=\"stationd_halted\", {halted})\n\
         safety = single(id=\"stationd_fallback\", {fallback})\n\
         startup = blank(id=\"stationd_startup\")\n\n\
         # Relay of a `remote` playlist: idle until stationd answers `relay`.\n\
         relay = input.http(id=\"stationd_relay\", start=false, {{stationd.relay_url()}})\n\
         def stationd.relay_on_fn(url) =\n  \
           if not stationd.relaying() or stationd.relay_url() != url then\n    \
             if relay.is_started() then relay.stop() end\n    \
             stationd.relay_url := url\n    \
             relay.start()\n    \
             stationd.relaying := true\n    \
             log.important(label=\"stationd\", \"relay: #{{url}}\")\n  \
           end\n\
         end\n\
         def stationd.relay_off_fn() =\n  \
           if stationd.relaying() then\n    \
             stationd.relaying := false\n    \
             relay.stop()\n    \
             log.important(label=\"stationd\", \"relay stopped\")\n  \
           end\n\
         end\n\
         stationd.relay_on := stationd.relay_on_fn\n\
         stationd.relay_off := stationd.relay_off_fn\n\
         # While relaying, keep asking stationd (the pull is not read then, so\n\
         # request.dynamic stops polling on its own): a `file` / `halted` /\n\
         # `none` answer ends the relay.\n\
         thread.run(fast=false, every={retry}, fun () ->\n  \
           if stationd.relaying() and list.length(pull_raw.queue()) == 0 then\n    \
             ignore(pull_raw.fetch())\n  \
           end\n\
         )\n\n\
         # Report Liquidsoap's own sources on every SWITCH to them, not on a track\n\
         # start: a noise loop left mid-way is resumed (no new track), a later stop\n\
         # would go unreported. Off the streaming thread (HTTP call).\n\
         def stationd.switched_to(kind, b) =\n  \
           thread.run(fast=false, {{stationd.report(\"\", kind)}})\n  \
           b\n\
         end\n\n\
         radio = fallback(\n  \
           id=\"stationd_air\",\n  \
           track_sensitive=false,\n  \
           transitions=[\n    \
             fun (_, b) -> stationd.switched_to(\"relay\", b),\n    \
             fun (_, b) -> b,\n    \
             fun (_, b) -> stationd.switched_to(\"halted\", b),\n    \
             fun (_, b) -> b,\n    \
             fun (_, b) -> stationd.switched_to(\"fallback\", b)\n  \
           ],\n  \
           [\n    \
             # soft entry: only once the pull — crossfade tail included — is\n    \
             # done (else the tail would be cut, then replayed at the exit)\n    \
             source.available(relay, {{stationd.relaying() and not stationd.paused() and not pull.is_ready()}}),\n    \
             source.available(pull, {{not stationd.paused()}}),\n    \
             source.available(halted_noise, {{stationd.halted() or stationd.paused()}}),\n    \
             source.available(startup, {{stationd.loading()}}),\n    \
             safety\n  \
           ]\n\
         )\n",
        halted = liq_path(&absolute(&ls.halted_path)),
        fallback = liq_path(&absolute(&ls.fallback_path)),
        retry = liq_float(PULL_RETRY_S),
    ));

    o.push_str(
        "\n# Hard overrides: pushed by stationd (`stationd.interrupt`), they cut the\n\
         # air now (track_sensitive=false) and give it back when they end.\n\
         interrupt = request.queue(id=\"stationd_interrupt\")\n\
         source.methods(interrupt).on_track(synchronous=false, fun (m) -> stationd.report(m[\"stationd_rid\"], \"\"))\n\
         radio = fallback(id=\"stationd_cut\", track_sensitive=false, [interrupt, radio])\n",
    );

    o.push_str(
        r##"
# ─── control commands (socket, namespace `stationd`) ──────────────────────
def stationd.cmd_pause(_) =
  stationd.paused := true
  log.important(label="stationd", "pause: halted noise on air, current track frozen")
  "OK"
end

def stationd.cmd_resume(_) =
  stationd.paused := false
  stationd.next_not_before := 0.
  log.important(label="stationd", "resume")
  "OK"
end

# The next track is normally asked for only near the end of the current one:
# fetch it NOW (urgent) before skipping, or the safety file would fill the gap.
def stationd.cmd_skip(_) =
  if list.length(pull_raw.queue()) == 0 then
    stationd.urgent := true
    stationd.next_not_before := 0.
    ignore(pull_raw.fetch())
  end
  source.skip(pull_raw)
  log.important(label="stationd", "skip")
  "OK"
end

# Drop the track already prepared (prefetched) so the next pull asks stationd
# again: a stop then takes effect at the end of the CURRENT track. A track the
# crossfade already started mixing can no longer be dropped.
def stationd.cmd_flush(_) =
  pull_raw.set_queue([])
  stationd.next_not_before := 0.
  log.important(label="stationd", "flush: prepared track dropped")
  "OK"
end

# Hard override: cut in now. The cut track is skipped: after the insert the
# prepared track plays (stationd flushes it beforehand when it should be
# re-asked — never when it is itself an override).
def stationd.cmd_interrupt(uri) =
  interrupt.push(request.create(uri))
  source.skip(pull_raw)
  log.important(label="stationd", "interrupt: hard override cut in")
  "OK"
end

def stationd.cmd_state(_) =
  "paused=#{stationd.paused()} halted=#{stationd.halted()} loading=#{stationd.loading()}"
end

server.register(namespace="stationd", usage="pause", description="Pause now (noise on air, track frozen).", "pause", stationd.cmd_pause)
server.register(namespace="stationd", usage="resume", description="Resume the frozen track.", "resume", stationd.cmd_resume)
server.register(namespace="stationd", usage="skip", description="Skip the current track.", "skip", stationd.cmd_skip)
server.register(namespace="stationd", usage="flush", description="Drop the prepared track (re-ask stationd).", "flush", stationd.cmd_flush)
server.register(namespace="stationd", usage="interrupt <uri>", description="Hard override: cut in now.", "interrupt", stationd.cmd_interrupt)
server.register(namespace="stationd", usage="state", description="Bridge flags.", "state", stationd.cmd_state)
"##,
    );

    if ls.normalize {
        o.push_str(
            "\n# Normalisation + compression\n\
             radio = normalize(target=0., window=0.03, gain_min=-16., gain_max=0., radio)\n\
             radio = compress.exponential(radio, mu=1.0)\n",
        );
    }


    if let Some(inc) = &ls.custom_include {
        o.push_str(&format!(
            "\n# ─── custom include ([liquidsoap] custom_include) ──────────────────────\n\
             %include \"{}\"\n",
            absolute(inc).to_string_lossy()
        ));
    }

    o.push_str("\n# ─── outputs ───────────────────────────────────────────────────────────\n");
    for (i, out) in ls.outputs.iter().enumerate() {
        o.push_str(&render_output(i + 1, out, station_name));
    }
    o
}

fn render_output(n: usize, out: &IcecastOutput, station_name: &str) -> String {
    let encoder = match out.format {
        OutputFormat::Mp3 => format!(
            "%ffmpeg(format=\"mp3\", %audio(codec=\"libmp3lame\", ac=2, ar=44100, b=\"{}k\"))",
            out.bitrate
        ),
    };
    let name = out.name.as_deref().unwrap_or(station_name);
    format!(
        "output.icecast(\n  \
           {encoder},\n  \
           id=\"stationd_out_{n}\",\n  \
           host={host},\n  \
           port={port},\n  \
           password={password},\n  \
           mount={mount},\n  \
           name={name},\n  \
           description={description},\n  \
           genre={genre},\n  \
           public={public},\n  \
           encoding=\"UTF-8\",\n  \
           radio\n\
         )\n",
        host = liq_string(&out.host),
        port = out.port,
        password = liq_string(&out.password),
        mount = liq_string(&out.mount),
        name = liq_string(name),
        description = liq_string(out.description.as_deref().unwrap_or("")),
        genre = liq_string(out.genre.as_deref().unwrap_or("")),
        public = out.public,
    )
}

/// Write the script only when its content changed (atomic: temp + rename).
/// Returns `true` when the file was (re)written — Liquidsoap must then be
/// restarted to pick it up.
pub fn write_if_changed(path: &Path, script: &str) -> std::io::Result<bool> {
    if let Ok(current) = std::fs::read_to_string(path) {
        if current == script {
            return Ok(false);
        }
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("liq.tmp");
    std::fs::write(&tmp, script)?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CrossfadeConfig, IcecastOutput};
    use std::path::PathBuf;

    fn cfg() -> LiquidsoapConfig {
        LiquidsoapConfig {
            script_path: PathBuf::from("/tmp/x.liq"),
            control_socket: PathBuf::from("/tmp/ls.sock"),
            http_bind: "127.0.0.1:8081".into(),
            api_token: "tok".into(),
            fallback_path: PathBuf::from("/srv/error.mp3"),
            halted_path: PathBuf::from("/srv/noise.mp3"),
            crossfade: CrossfadeConfig::default(),
            normalize: false,
            custom_include: None,
            log_level: 3,
            outputs: vec![IcecastOutput {
                host: "127.0.0.1".into(),
                port: 8000,
                password: "hack\"me".into(),
                mount: "/radio.mp3".into(),
                format: OutputFormat::Mp3,
                bitrate: 192,
                name: None,
                description: Some("Pre production radio".into()),
                genre: None,
                public: false,
            }],
        }
    }

    #[test]
    fn strings_are_escaped_and_never_interpolated() {
        assert_eq!(liq_string("plain"), "\"plain\"");
        assert_eq!(liq_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(liq_string("x#{y}"), "(\"x#\" ^ \"{y}\")");
        assert_eq!(liq_string("#{a}#{b}"), "(\"#\" ^ \"{a}#\" ^ \"{b}\")");
        assert_eq!(liq_string("l1\nl2"), "\"l1\\nl2\"");
    }

    #[test]
    fn floats_always_have_a_point() {
        assert_eq!(liq_float(2.0), "2.");
        assert_eq!(liq_float(0.5), "0.5");
    }

    #[test]
    fn renders_the_bridge_chain_and_outputs() {
        let s = render(&cfg(), "Ma Radio");
        assert!(s.contains("let stationd.api_url = \"http://127.0.0.1:8081/ls/v1\""));
        assert!(s.contains("let stationd.api_token = \"tok\""));
        assert!(s.contains("request.dynamic(id=\"stationd_pull\""));
        assert!(s.contains("stationd.next_not_before := time() + 2."));
        assert!(s.contains("settings.server.socket.path := \"/tmp/ls.sock\""));
        assert!(s.contains("settings.server.socket.permissions := 0o660"));
        assert!(s.contains("source.available(halted_noise, {stationd.halted() or stationd.paused()})"));
        assert!(s.contains("pull_raw.set_queue([])"));
        assert!(s.contains("interrupt = request.queue(id=\"stationd_interrupt\")"));
        assert!(s.contains("fallback(id=\"stationd_cut\", track_sensitive=false, [interrupt, radio])"));
        // the cut sits above the whole air chain, before the outputs
        assert!(s.find("stationd_cut").unwrap() > s.find("stationd_air").unwrap());
        assert!(s.find("stationd_cut").unwrap() < s.find("output.icecast").unwrap());
        for cmd in ["pause", "resume", "skip", "flush", "state"] {
            assert!(s.contains(&format!("namespace=\"stationd\", usage=\"{cmd}\"")), "{cmd}");
        }
        assert!(s.contains("usage=\"interrupt <uri>\""));
        assert!(s.contains("source.skip(pull_raw)"));
        assert!(s.contains("cross(id=\"stationd_cross\", duration=3."));
        assert!(s.contains("fade_in=2., fade_out=2."));
        assert!(s.contains("single(id=\"stationd_halted\", \"/srv/noise.mp3\")"));
        // own sources reported on every switch (fallback transitions), never
        // on a track start (a resumed noise loop starts no track)
        assert!(s.contains("fun (_, b) -> stationd.switched_to(\"halted\", b)"));
        assert!(!s.contains("source.methods(halted_noise).on_track"));
        assert!(s.contains("single(id=\"stationd_fallback\", \"/srv/error.mp3\")"));
        assert!(s.contains("fun (_, b) -> stationd.switched_to(\"fallback\", b)"));
        assert!(!s.contains("source.methods(safety).on_track"));
        // one transition per fallback member, in the same order
        let tr = s.find("transitions=[").unwrap();
        let members = s.find("source.available(relay").unwrap();
        assert_eq!(s[tr..members].matches("fun (_, b)").count(), 5);
        assert!(!s.contains("\"annotate:"), "annotate: makes single() fallible");
        // pull first, halted noise before the safety fallback.
        let pull = s.find("source.available(pull, {not stationd.paused()})").unwrap();
        let halted = s.find("source.available(halted_noise").unwrap();
        let safety = s.find("    safety\n").unwrap();
        assert!(pull < halted && halted < safety);
        // Track starts observed on the pull BEFORE the crossfade.
        let report = s.find("source.methods(pull).on_track(synchronous=false, stationd.pull_started)").unwrap();
        assert!(report < s.find("cross(id=").unwrap());
        assert!(s.contains("b=\"192k\""));
        assert!(s.contains("password=\"hack\\\"me\""));
        assert!(s.contains("name=\"Ma Radio\""));
        assert!(s.contains("description=\"Pre production radio\""));
        assert!(s.contains("public=false"));
        assert!(!s.contains("normalize("));
        assert!(!s.contains("%include"));
    }

    #[test]
    fn the_next_track_is_asked_for_near_the_end_of_the_current_one() {
        // lead = crossfade overlap + one retry delay + margin.
        assert_eq!(pull_lead_s(&cfg()), 3.0 + 2.0 + 2.0);
        let mut none = cfg();
        none.crossfade.mode = CrossfadeMode::None;
        assert_eq!(pull_lead_s(&none), 4.0);

        let s = render(&cfg(), "R");
        assert!(s.contains("let stationd.lead = 7."));
        assert!(s.contains("let stationd.urgent = ref(true)"));
        // The gate: too early → nothing asked (no HTTP call), unless urgent.
        let gate = s.find("if not stationd.urgent() and left > stationd.lead then").unwrap();
        let ask = s.find("r = stationd.fetch_next()").unwrap();
        assert!(gate < ask);
        assert!(s[gate..ask].contains("stationd.urgent := false"));
        // The remaining time comes from the pull source once it exists.
        let pull = s.find("pull = request.dynamic(").unwrap();
        let wired = s.find("stationd.remaining := fun () -> pull_raw.remaining()").unwrap();
        assert!(pull < wired);
        // A skip fetches the next track (urgent) BEFORE skipping.
        let skip = s.find("def stationd.cmd_skip(_) =").unwrap();
        let body = &s[skip..s[skip..].find("\nend\n").unwrap() + skip];
        let fetch = body.find("ignore(pull_raw.fetch())").unwrap();
        assert!(body.find("stationd.urgent := true").unwrap() < fetch);
        assert!(fetch < body.find("source.skip(pull_raw)").unwrap());
        assert!(body.contains("if list.length(pull_raw.queue()) == 0 then"));
    }

    #[test]
    fn a_remote_playlist_is_relayed_by_input_http() {
        let s = render(&cfg(), "R");
        // An idle relay source, fed with the URL stationd hands out.
        assert!(s.contains(
            "relay = input.http(id=\"stationd_relay\", start=false, {stationd.relay_url()})"
        ));
        // First in the air chain, but only once the (crossfaded) pull is done:
        // a soft entry, and no crossfade tail cut then replayed.
        let relay = s.find("source.available(relay, {stationd.relaying() and not stationd.paused() and not pull.is_ready()})").unwrap();
        let pull = s.find("source.available(pull, {not stationd.paused()})").unwrap();
        assert!(relay < pull);
        assert!(s.contains("fun (_, b) -> stationd.switched_to(\"relay\", b)"));
        // Replies: `relay` starts it and queues nothing; `halted` / `none`
        // stop it at once; a `file` lets it yield when the track is ready and
        // stop when the track starts (pull_started).
        let relay_branch = s.find("elsif kind == \"relay\" then").unwrap();
        let halted_branch = s.find("elsif kind == \"halted\" then").unwrap();
        assert!(s[relay_branch..halted_branch].contains("relay_on(uri)"));
        assert!(s[relay_branch..halted_branch].contains("null"));
        let tail = &s[halted_branch..s[halted_branch..].find("catch err do").unwrap() + halted_branch];
        assert_eq!(tail.matches("relay_off = stationd.relay_off()").count(), 2, "halted and none");
        let started = s.find("def stationd.pull_started(m) =").unwrap();
        assert!(s[started..started + 200].contains("relay_off()"));
        // While relaying, the script keeps asking stationd itself.
        assert!(s.contains("thread.run(fast=false, every=2., fun () ->"));
        assert!(s.contains("if stationd.relaying() and list.length(pull_raw.queue()) == 0 then"));
    }

    #[test]
    fn optional_parts() {
        let mut c = cfg();
        c.crossfade.mode = CrossfadeMode::None;
        c.normalize = true;
        c.custom_include = Some(PathBuf::from("/etc/stationd/custom.liq"));
        c.outputs.push(IcecastOutput { mount: "/low.mp3".into(), bitrate: 64, ..c.outputs[0].clone() });
        let s = render(&c, "R");
        assert!(!s.contains("cross("));
        assert!(s.contains("compress.exponential"));
        assert!(s.contains("%include \"/etc/stationd/custom.liq\""));
        // the include comes before the outputs (it may reassign `radio`).
        assert!(s.find("%include").unwrap() < s.find("output.icecast").unwrap());
        assert!(s.contains("id=\"stationd_out_2\""));
        assert!(s.contains("b=\"64k\""));
    }

    #[test]
    fn write_only_when_changed() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/station.liq");
        assert!(write_if_changed(&p, "a").unwrap());
        assert!(!write_if_changed(&p, "a").unwrap());
        assert!(write_if_changed(&p, "b").unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "b");
    }
}
