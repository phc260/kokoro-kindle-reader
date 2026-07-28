// Fixture-provided globals (test/ocr-fixture.html).
declare global {
  interface Window {
    GROUND_TRUTH: string;
    renderPage: (opts?: { columns?: number; dark?: boolean; font?: string; size?: number; footer?: boolean }) => Promise<Blob>;
    benchReady: boolean;
  }
}
export {};

declare global {
  interface Window {
    TRUTH: string;
    ready: boolean;
  }
}
