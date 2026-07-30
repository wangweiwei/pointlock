// @pointlock/ui entry: hash routing (no server-side SPA fallback needed —
// the Rust host serves static files as-is), token captured from the
// startup-printed URL (08 §1 iron law 4). Route params follow 08 §2.1;
// `:runPath` is the §9 canonical string, URL-encoded — deep links ARE
// locate hyperlinks.

import { StrictMode, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { HashRouter, Link, Route, Routes, Navigate } from "react-router-dom";
import { FlowsPage } from "./pages/FlowsPage";
import { FlowDetailPage } from "./pages/FlowDetailPage";
import { RunPage } from "./pages/RunPage";
import { InboxPage } from "./pages/InboxPage";
import { api, subscribeRevision, token } from "./api";
import "./theme.css";

/** The §3.5 third linkage point: pending requests light the inbox dot. */
function InboxLink() {
  const [revision, setRevision] = useState(0);
  const [pending, setPending] = useState(0);
  useEffect(() => subscribeRevision(null, setRevision), []);
  useEffect(() => {
    let alive = true;
    api
      .inbox()
      .then((body) => alive && setPending(body.inbox.length))
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [revision]);
  return (
    <Link to="/inbox">
      inbox{pending > 0 && <span className="badge-dot" title={`${pending} pending`} />}
    </Link>
  );
}

function App() {
  return (
    <HashRouter>
      <div className="topbar">
        <span className="brand">Pointlock</span>
        <Link to="/flows">flows</Link>
        <InboxLink />
        <span className="spacer" />
        <span style={{ color: "var(--muted)" }}>
          read-only projection console — runs start and respond via the CLI
        </span>
      </div>
      <Routes>
        <Route path="/" element={<Navigate to="/flows" replace />} />
        <Route path="/flows" element={<FlowsPage />} />
        <Route path="/flows/:flowId" element={<FlowDetailPage />} />
        <Route path="/runs/:runId/steps/*" element={<RunPage />} />
        <Route path="/runs/:runId" element={<RunPage />} />
        <Route path="/inbox" element={<InboxPage />} />
      </Routes>
    </HashRouter>
  );
}

token(); // capture ?token= before any navigation drops the query
createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
