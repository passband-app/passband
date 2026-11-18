// First, before React: an explicit theme choice goes onto <html> now.
import "./app/theme-boot";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./ui/App";

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}
