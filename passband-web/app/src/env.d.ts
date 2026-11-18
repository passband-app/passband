/// <reference types="vite/client" />

/** True only in dev and e2e builds: a loopback daemon may be paired with. */
declare const __ALLOW_LOOPBACK__: boolean;

interface ImportMetaEnv {
  /** Control's sign-in entry point. */
  readonly VITE_CONTROL_URL?: string;
  /** Where "Get Passband for Mac" points. */
  readonly VITE_DESKTOP_DOWNLOAD_URL?: string;
}
