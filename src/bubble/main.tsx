import React from "react";
import ReactDOM from "react-dom/client";
import { Bubble } from "./Bubble";

// Do NOT import index.css here — it sets body background colors
// that break the transparent window. The bubble uses inline styles only.

ReactDOM.createRoot(document.getElementById("bubble-root")!).render(
  <React.StrictMode>
    <Bubble />
  </React.StrictMode>
);
