import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./design/tokens.css";
import "./design/components.css";
import "./app.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
