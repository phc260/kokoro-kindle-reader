import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { createTheme, CssBaseline, ThemeProvider } from "@mui/material";

export type ColorMode = "light" | "dark";

interface ColorModeContextValue {
  mode: ColorMode;
  toggle: () => void;
  // Page-background warmth, 0 (cool) … 1 (warm).
  temperature: number;
  setTemperature: (t: number) => void;
}

const ColorModeContext = createContext<ColorModeContextValue>({
  mode: "light",
  toggle: () => {},
  temperature: 0.6,
  setTemperature: () => {},
});

// Light/dark state for the app. Read it (and the toggle) anywhere with this hook.
export const useColorMode = () => useContext(ColorModeContext);

// Start from the user's saved choice; on first run, follow the OS preference.
function initialMode(): ColorMode {
  const saved = localStorage.getItem("theme-mode");
  if (saved === "light" || saved === "dark") return saved;
  return window.matchMedia?.("(prefers-color-scheme: dark)").matches
    ? "dark"
    : "light";
}

// Warm-ish by default so the page reads as off-white paper rather than glare.
const DEFAULT_TEMPERATURE = 0.6;

function initialTemperature(): number {
  const raw = localStorage.getItem("color-temp");
  const n = raw == null ? NaN : Number(raw);
  return Number.isFinite(n) && n >= 0 && n <= 1 ? n : DEFAULT_TEMPERATURE;
}

type Rgb = readonly [number, number, number];

// Cool↔warm page-background endpoints per mode, kept at a near-constant lightness
// so the slider only shifts hue/warmth, not brightness.
const BG_ENDPOINTS: Record<ColorMode, { cool: Rgb; warm: Rgb }> = {
  // Within each mode the cool/warm pair shares an average lightness (~246 in
  // light, ~15 in dark), so the slider shifts only hue. Light sits a hair below
  // pure white to avoid glare; dark sits just above pure black.
  light: { cool: [243, 246, 250], warm: [252, 247, 239] },
  dark: { cool: [12, 14, 18], warm: [18, 15, 11] },
};

function mix(a: Rgb, b: Rgb, t: number): string {
  const c = (i: number) => Math.round(a[i] + (b[i] - a[i]) * t);
  return `rgb(${c(0)}, ${c(1)}, ${c(2)})`;
}

// Interpolated page background for the given mode and warmth.
function backgroundFor(mode: ColorMode, t: number): string {
  const { cool, warm } = BG_ENDPOINTS[mode];
  return mix(cool, warm, t);
}

// Wraps the app in an MUI theme whose palette mode the user can flip. CssBaseline
// makes the window background follow the mode too. The choice persists across
// launches in localStorage.
export function ColorModeProvider({ children }: { children: ReactNode }) {
  const [mode, setMode] = useState<ColorMode>(initialMode);
  const [temperature, setTemperature] = useState<number>(initialTemperature);

  useEffect(() => {
    localStorage.setItem("theme-mode", mode);
  }, [mode]);

  useEffect(() => {
    localStorage.setItem("color-temp", String(temperature));
  }, [temperature]);

  const value = useMemo<ColorModeContextValue>(
    () => ({
      mode,
      toggle: () => setMode((m) => (m === "light" ? "dark" : "light")),
      temperature,
      setTemperature,
    }),
    [mode, temperature],
  );

  const theme = useMemo(
    () =>
      createTheme({
        palette: {
          mode,
          // The page background tracks the Color-temperature slider (cool↔warm).
          // MUI's default light page is pure #fff (glare on a full-screen
          // reader), so keep `paper` (menus, buttons) crisp white in light mode
          // while the page itself takes the warmer tone.
          background:
            mode === "light"
              ? { default: backgroundFor("light", temperature), paper: "#ffffff" }
              : { default: backgroundFor("dark", temperature) },
        },
      }),
    [mode, temperature],
  );

  return (
    <ColorModeContext.Provider value={value}>
      <ThemeProvider theme={theme}>
        <CssBaseline />
        {children}
      </ThemeProvider>
    </ColorModeContext.Provider>
  );
}
