import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openPath, openUrl } from "@tauri-apps/plugin-opener";
import { useNavigate } from "react-router-dom";
import {
  Box,
  Button,
  CircularProgress,
  LinearProgress,
  Link,
  Typography,
} from "@mui/material";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";

interface DownloadProgress {
  downloaded: number;
  total: number;
  file: string;
}

// The Apache-2.0 model the voices are downloaded from (see
// src-tauri/model-manifest.json), shown as the source on the setup screen.
const MODEL_REPO = "onnx-community/Kokoro-82M-v1.0-ONNX";
const MODEL_REPO_URL = `https://huggingface.co/${MODEL_REPO}`;

function formatMB(bytes: number): string {
  return `${(bytes / 1024 / 1024).toFixed(0)} MB`;
}

// First-run wizard: download the TTS model. It is required before the reader
// (the "/" route) will open — main.tsx's AppGate redirects here until
// `model_exists` is true.
function Setup() {
  const navigate = useNavigate();

  // null = still checking on mount; true/false = present or not.
  const [modelReady, setModelReady] = useState<boolean | null>(null);
  const [modelPath, setModelPath] = useState("");

  // Model download state.
  const [downloading, setDownloading] = useState(false);
  const [downloadError, setDownloadError] = useState("");
  const [progress, setProgress] = useState<DownloadProgress | null>(null);

  useEffect(() => {
    invoke<boolean>("model_exists").then(setModelReady).catch(() => setModelReady(false));
    invoke<string>("model_location").then(setModelPath).catch(() => {});
  }, []);

  // Listen for download progress for the lifetime of the screen.
  useEffect(() => {
    const unlisten = listen<DownloadProgress>("model-download-progress", (e) =>
      setProgress(e.payload),
    );
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // Once the model is downloaded, enter the app.
  const navigatedRef = useRef(false);
  useEffect(() => {
    if (modelReady && !navigatedRef.current) {
      navigatedRef.current = true;
      navigate("/", { replace: true });
    }
  }, [modelReady, navigate]);

  async function handleDownload() {
    setDownloading(true);
    setDownloadError("");
    try {
      await invoke("download_model");
      setModelReady(true);
    } catch (e) {
      setDownloadError(String(e));
    } finally {
      setDownloading(false);
    }
  }

  // Still checking what's already present.
  if (modelReady === null) {
    return (
      <Box
        sx={{
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          minHeight: "100vh",
        }}
      >
        <CircularProgress />
      </Box>
    );
  }

  const percent =
    progress && progress.total > 0
      ? Math.min(100, (progress.downloaded / progress.total) * 100)
      : 0;

  return (
    <Box
      sx={{
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        minHeight: "60vh",
        gap: 3,
        p: 4,
        textAlign: "center",
      }}
    >
      <Typography variant="h5">Set up your reader</Typography>

      {/* Download the TTS model */}
      <Box
        sx={{
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          gap: 1.5,
          width: "100%",
          maxWidth: 460,
        }}
      >
        {modelReady ? (
          <Typography color="success.main" sx={{ display: "flex", alignItems: "center", gap: 1 }}>
            <CheckCircleIcon fontSize="small" /> Reading voices ready
          </Typography>
        ) : downloading ? (
          <>
            <Typography color="text.secondary">
              Downloading reading voices and models…{" "}
              {progress
                ? `${formatMB(progress.downloaded)} / ${formatMB(progress.total)}`
                : "starting…"}
            </Typography>
            <LinearProgress
              variant={progress ? "determinate" : "indeterminate"}
              value={percent}
              sx={{ width: "100%" }}
            />
            {progress && (
              <Typography
                variant="caption"
                color="text.secondary"
                noWrap
                sx={{ maxWidth: "100%" }}
              >
                {progress.file}
              </Typography>
            )}
          </>
        ) : (
          <>
            <Typography color="text.secondary" sx={{ maxWidth: 460 }}>
              The text-to-speech voices and models (about 430 MB) are downloaded once and
              stored on this device, so reading aloud works offline afterward.
            </Typography>
            <Button variant="contained" onClick={() => void handleDownload()}>
              Download (~430 MB)
            </Button>
            {downloadError && (
              <Typography color="error" variant="body2" sx={{ maxWidth: 460 }}>
                {downloadError}
              </Typography>
            )}
            <Typography variant="caption" color="text.secondary">
              Source:{" "}
              <Link
                component="button"
                type="button"
                onClick={() => void openUrl(MODEL_REPO_URL)}
                sx={{ verticalAlign: "baseline" }}
              >
                {MODEL_REPO}
              </Link>{" "}
              on Hugging Face
            </Typography>
          </>
        )}

        {!modelReady && modelPath && (
          <Typography
            variant="caption"
            color="text.secondary"
            noWrap
            sx={{ maxWidth: "100%" }}
          >
            Saved to:{" "}
            <Link
              component="button"
              type="button"
              onClick={() => void openPath(modelPath)}
              sx={{ verticalAlign: "baseline" }}
            >
              {modelPath}
            </Link>
          </Typography>
        )}
      </Box>
    </Box>
  );
}

export default Setup;
