import { useEffect, useRef, useState } from "react";
import {
  Box,
  Button,
  CircularProgress,
  MenuItem,
  TextField,
  Typography,
} from "@mui/material";
import { initTTS, stopTTS, synthesize } from "./tts";
import { VOICES, loadVoice } from "./voices";
import "./App.css";

const SAMPLE_TEXT =
  "Kokoro reader is alive! This text is synthesized locally in the browser by " +
  "the Kokoro model running through kokoro.js. Pick a voice, then press play.";

type Backend = "webgpu" | "wasm";

function App() {
  const [text, setText] = useState(SAMPLE_TEXT);
  const [voice, setVoice] = useState(loadVoice());
  const [ready, setReady] = useState(false);
  const [backend, setBackend] = useState<Backend | "">("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const audioRef = useRef<HTMLAudioElement | null>(null);
  const urlRef = useRef<string>("");

  useEffect(() => {
    initTTS((b) => {
      setReady(true);
      setBackend(b);
    });
    return () => {
      if (urlRef.current) URL.revokeObjectURL(urlRef.current);
    };
  }, []);

  useEffect(() => {
    localStorage.setItem("tts-voice", voice);
  }, [voice]);

  async function play() {
    setError("");
    setBusy(true);
    try {
      const url = await synthesize(text, voice);
      if (!url) return; // superseded or stopped
      if (urlRef.current) URL.revokeObjectURL(urlRef.current);
      urlRef.current = url;
      const audio = audioRef.current ?? new Audio();
      audioRef.current = audio;
      audio.src = url;
      await audio.play();
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
  }

  return (
    <Box sx={{ display: "flex", flexDirection: "column", gap: 2, p: 3 }}>
      <Typography variant="h5">kokoro-reader</Typography>

      <TextField
        multiline
        minRows={6}
        value={text}
        onChange={(e) => setText(e.target.value)}
        placeholder="Paste text to read aloud…"
        fullWidth
      />

      <Box sx={{ display: "flex", flexWrap: "wrap", alignItems: "center", gap: 2 }}>
        <TextField
          select
          label="Narrator"
          size="small"
          value={voice}
          onChange={(e) => setVoice(e.target.value)}
          sx={{ minWidth: 220 }}
          disabled={!ready}
        >
          {VOICES.map((v) => (
            <MenuItem key={v.id} value={v.id}>
              {v.name} — {v.group}
            </MenuItem>
          ))}
        </TextField>

        <Button
          variant="contained"
          onClick={play}
          disabled={!ready || busy}
          startIcon={busy ? <CircularProgress size={18} color="inherit" /> : undefined}
        >
          {busy ? "Synthesizing…" : "▶ Play"}
        </Button>
        <Button variant="outlined" onClick={stop} disabled={!ready}>
          ■ Stop
        </Button>
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
