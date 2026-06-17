import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import {
  Box,
  Button,
  CircularProgress,
  FormControl,
  ListSubheader,
  MenuItem,
  Select,
  Slider,
  ToggleButton,
  ToggleButtonGroup,
  Tooltip,
  Typography,
} from "@mui/material";
import GraphicEqIcon from "@mui/icons-material/GraphicEq";
import PlayArrowIcon from "@mui/icons-material/PlayArrow";
import RecordVoiceOverIcon from "@mui/icons-material/RecordVoiceOver";
import StopIcon from "@mui/icons-material/Stop";
import { invoke } from "@tauri-apps/api/core";
import { initTTS, stopTTS, synthesize } from "./tts";
import { VOICES, loadVoice, voiceIntro } from "./voices";
import "./App.css";

type Backend = "webgpu" | "wasm";

function loadNum(key: string, def: number): number {
  const v = parseFloat(localStorage.getItem(key) ?? "");
  return Number.isFinite(v) ? v : def;
}

// Icon-only transport button: an outlined Button wrapped in a Tooltip, with the
// icon as children. The span keeps the Tooltip working while the button is
// disabled (MUI can't attach a listener to a disabled control directly).
function ControlButton({
  label,
  onClick,
  disabled,
  color = "primary",
  children,
}: {
  label: string;
  onClick?: () => void;
  disabled?: boolean;
  color?: "primary" | "warning" | "error";
  children: ReactNode;
}) {
  return (
    <Tooltip title={label}>
      <span>
        <Button
          variant="outlined"
          color={color}
          aria-label={label}
          onClick={onClick}
          disabled={disabled}
          sx={{ minWidth: 0, p: 1, borderRadius: 1 }}
        >
          {children}
        </Button>
      </span>
    </Tooltip>
  );
}

function App() {
  const [voice, setVoice] = useState(loadVoice());
  const [speed, setSpeed] = useState(() => loadNum("tts-speed", 1));
  const [gain, setGain] = useState(() => loadNum("tts-gain", 1));
  const [ready, setReady] = useState(false);
  const [backend, setBackend] = useState<Backend | "">("");
  const [busy, setBusy] = useState(false); // synthesizing
  const [playing, setPlaying] = useState(false); // audio is sounding
  // Which voice agency Kindle is set to: "none" (unset — neither segment shown),
  // "microsoft", or "kokoro". Drives the toggle and gates the Kokoro controls.
  const [agency, setAgency] = useState(
    () => localStorage.getItem("kindle-agency") ?? "none",
  );
  const kokoro = agency === "kokoro";
  const [error, setError] = useState("");

  const audioRef = useRef<HTMLAudioElement | null>(null);
  const urlRef = useRef<string>("");

  useEffect(() => {
    initTTS((b) => {
      setReady(true);
      setBackend(b);
    });
    // Reflect Kindle's recorded voice agency in the toggle ("none" | "microsoft"
    // | "kokoro", from controls.ini). Defaults to "none" if it can't be read.
    invoke<string>("kindle_voice")
      .then((v) => {
        setAgency(v);
        localStorage.setItem("kindle-agency", v);
      })
      .catch((e) => console.debug("[kindle voice] detect skipped:", e));
    return () => {
      if (urlRef.current) URL.revokeObjectURL(urlRef.current);
    };
  }, []);

  // Persist the controls and push them to the SAPI engine (controls.ini), so the
  // narrator/speed/gain also drive Kindle. Ignored if the voice isn't registered.
  useEffect(() => {
    localStorage.setItem("tts-voice", voice);
    localStorage.setItem("tts-speed", String(speed));
    localStorage.setItem("tts-gain", String(gain));
    invoke("set_controls", { voice, speed, gain }).catch((e) =>
      console.debug("[controls] set_controls skipped:", e),
    );
  }, [voice, speed, gain]);

  async function play() {
    setError("");
    setBusy(true);
    try {
      const url = await synthesize(voiceIntro(voice), voice, speed);
      if (!url) return; // superseded or stopped
      if (urlRef.current) URL.revokeObjectURL(urlRef.current);
      urlRef.current = url;
      const audio = audioRef.current ?? new Audio();
      audioRef.current = audio;
      audio.src = url;
      audio.volume = Math.min(gain, 1); // preview gain (media volume can't boost > 1)
      audio.onended = () => setPlaying(false);
      await audio.play();
      setPlaying(true);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  function stop() {
    stopTTS();
    audioRef.current?.pause();
    setBusy(false);
    setPlaying(false);
  }

  // Switch Kindle's voice agency to "microsoft" or "kokoro". The backend runs the
  // guard elevated (UAC) and resolves only once it succeeds, so we apply the
  // selection optimistically but revert it if the switch is cancelled or fails.
  // Kindle must be reopened to actually apply the change.
  function selectAgency(next: "microsoft" | "kokoro") {
    const prev = agency;
    if (next !== "kokoro") stop(); // Kokoro controls go inactive; halt any preview
    setAgency(next);
    invoke("set_kindle_voice", { kokoro: next === "kokoro" })
      .then(() => localStorage.setItem("kindle-agency", next))
      .catch((e) => {
        setAgency(prev); // UAC cancelled / guard failed
        setError(String(e));
      });
  }

  return (
    <Box sx={{ display: "flex", flexDirection: "column", gap: 2, p: 3 }}>
      <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
        <Tooltip title="Voice">
          <GraphicEqIcon fontSize="small" color="action" />
        </Tooltip>
        <ToggleButtonGroup
          exclusive
          size="small"
          color="primary"
          value={agency}
          onChange={(_, val) => {
            if (val !== null) selectAgency(val);
          }}
          sx={{
            "& .MuiToggleButton-root.Mui-selected": {
              bgcolor: "primary.main",
              color: "primary.contrastText",
              "&:hover": { bgcolor: "primary.dark" },
            },
          }}
        >
          <ToggleButton value="microsoft">Microsoft</ToggleButton>
          <ToggleButton value="kokoro">Kokoro</ToggleButton>
        </ToggleButtonGroup>
      </Box>

      <Box sx={{ display: "flex", flexWrap: "wrap", alignItems: "center", gap: 2 }}>
        <Box sx={{ display: "flex", alignItems: "center", gap: 1, width: 220 }}>
          <RecordVoiceOverIcon fontSize="small" color="action" />
          <FormControl size="small" fullWidth>
            <Select
              aria-label="Narrator"
              value={voice}
              onChange={(e) => setVoice(e.target.value)}
              disabled={!ready || !kokoro}
              MenuProps={{ slotProps: { paper: { sx: { maxHeight: 360 } } } }}
            >
              {VOICES.flatMap((v, i) => {
                const items = [];
                if (i === 0 || v.group !== VOICES[i - 1].group) {
                  items.push(
                    <ListSubheader key={v.group}>{v.group}</ListSubheader>,
                  );
                }
                items.push(
                  <MenuItem key={v.id} value={v.id}>
                    {v.name}
                  </MenuItem>,
                );
                return items;
              })}
            </Select>
          </FormControl>
        </Box>

        {busy || playing ? (
          <ControlButton
            label={busy ? "Synthesizing…" : "Stop"}
            onClick={stop}
            color={busy ? "primary" : "error"}
          >
            {busy ? <CircularProgress size={24} color="inherit" /> : <StopIcon />}
          </ControlButton>
        ) : (
          <ControlButton
            label="Play"
            onClick={play}
            disabled={!ready || !kokoro}
          >
            <PlayArrowIcon />
          </ControlButton>
        )}
      </Box>

      <Box sx={{ display: "flex", flexWrap: "wrap", gap: 3 }}>
        <Box sx={{ width: 220 }}>
          <Typography variant="caption" color="text.secondary">
            Speed — {Math.round(speed * 100)}%
          </Typography>
          <Slider
            size="small"
            value={speed}
            min={0.5}
            max={2}
            step={0.05}
            disabled={!kokoro}
            onChange={(_, v) => setSpeed(v as number)}
          />
        </Box>
        <Box sx={{ width: 220 }}>
          <Typography variant="caption" color="text.secondary">
            Volume — {Math.round(gain * 100)}%
          </Typography>
          <Slider
            size="small"
            value={gain}
            min={0}
            max={2}
            step={0.05}
            disabled={!kokoro}
            onChange={(_, v) => setGain(v as number)}
          />
        </Box>
      </Box>

      <Typography variant="body2" color={error ? "error" : "text.secondary"}>
        {error
          ? error
          : ready
            ? `engine: kokoro.js (${backend})`
            : "loading model…"}
      </Typography>
    </Box>
  );
}

export default App;
