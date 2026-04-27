import { FormEvent, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// After the EventBus refactor (see OrionII/docs/ADR-001), the chat surface
// no longer awaits a `ChatExchange` return value from `send_chat_message`.
// The command returns immediately with a correlation id; the assistant
// reply arrives asynchronously on the `orion://ego/action` Tauri event,
// emitted by the UI emitter subscriber on `Topic::EgoAction`.

type ChatAck = {
  correlationId: string;
  accepted: boolean;
};

type EgoActionEvent = {
  correlationId: string | null;
  userQuery: string;
  responseText: string;
};

type ModelStatus = {
  role: string;
  provider: string;
  state: string;
  model: string;
  message: string | null;
};

type SecurityHealth = {
  constitutionalIntegrity: string;
  checkedAt: string;
  remediation: string | null;
};

type CompanionStatusReport = {
  companionId: string;
  persistedMessages: number;
  saoBacklog: number;
  policyVersion: number;
  memoryCount: number;
  security: SecurityHealth;
  modelStatus: ModelStatus[];
};

type TranscriptMessage = {
  id: string;
  role: "user" | "orion";
  text: string;
  topic: string;
  correlationId: string;
};

type ShipReport = {
  attempted: number;
  acked: number;
  failed: number;
};

type SaoConnectionStatus = {
  configured: boolean;
  baseUrl: string | null;
  agentId: string | null;
  birthed: boolean;
  agentName: string | null;
  ownerUsername: string | null;
  provider: string | null;
  idModel: string | null;
  egoModel: string | null;
  birthedAt: string | null;
  policyVersion: number | null;
  birthError: string | null;
  busTransport: string;
};

const EMPTY_STATUS: CompanionStatusReport = {
  companionId: "not loaded",
  persistedMessages: 0,
  saoBacklog: 0,
  policyVersion: 1,
  memoryCount: 0,
  security: {
    constitutionalIntegrity: "notChecked",
    checkedAt: "",
    remediation: null
  },
  modelStatus: []
};

const ORION_EGO_ACTION_EVENT = "orion://ego/action";

function App() {
  const [draft, setDraft] = useState("");
  const [history, setHistory] = useState<TranscriptMessage[]>([]);
  const [status, setStatus] = useState("M0 entity bus ready");
  const [companionStatus, setCompanionStatus] =
    useState<CompanionStatusReport>(EMPTY_STATUS);
  const [error, setError] = useState<string | null>(null);
  const [isSending, setIsSending] = useState(false);
  const [isSyncing, setIsSyncing] = useState(false);
  const [saoConnection, setSaoConnection] = useState<SaoConnectionStatus>({
    configured: false,
    baseUrl: null,
    agentId: null,
    birthed: false,
    agentName: null,
    ownerUsername: null,
    provider: null,
    idModel: null,
    egoModel: null,
    birthedAt: null,
    policyVersion: null,
    birthError: null,
    busTransport: "in_memory"
  });
  const [syncStatus, setSyncStatus] = useState("SAO sync not checked");
  const [lastShipReport, setLastShipReport] = useState<ShipReport | null>(null);
  const canSend = useMemo(() => draft.trim().length > 0 && !isSending, [draft, isSending]);

  useEffect(() => {
    invoke<SaoConnectionStatus>("sao_connection_status")
      .then((connection) => {
        setSaoConnection(connection);
        if (connection.birthed) {
          setSyncStatus(
            `Birthed as ${connection.agentName ?? "(unnamed)"} via ${connection.provider ?? "(no provider)"} (policy v${connection.policyVersion ?? 0})`
          );
        } else if (connection.configured) {
          setSyncStatus(
            `Enrollment anchor loaded at ${connection.baseUrl}; ${connection.birthError ? "SAO rejected it" : "SAO is unreachable"}`
          );
        } else {
          setSyncStatus(
            "Offline local mode; install OrionII from a SAO agent bundle to enroll"
          );
        }
      })
      .catch((cause) => setSyncStatus(`SAO status unavailable: ${String(cause)}`));

    invoke<CompanionStatusReport>("companion_status")
      .then(setCompanionStatus)
      .catch(() => {
        // Non-fatal: status will populate after the first ego.action event.
      });
  }, []);

  // Subscribe to `orion://ego/action` once at mount. This is the architectural
  // inversion: chat output flows through the bus, not through a command return.
  useEffect(() => {
    const unlistenPromise = listen<EgoActionEvent>(ORION_EGO_ACTION_EVENT, (event) => {
      const payload = event.payload;
      setHistory((current) => [
        ...current,
        {
          id: `${payload.correlationId ?? crypto.randomUUID()}-orion`,
          role: "orion",
          text: payload.responseText,
          topic: "ego.action",
          correlationId: payload.correlationId ?? "(unlinked)"
        }
      ]);
      setIsSending(false);

      // Refresh persistence-derived status after each ego response.
      invoke<CompanionStatusReport>("companion_status")
        .then(setCompanionStatus)
        .catch(() => {
          // Non-fatal.
        });
    });

    return () => {
      unlistenPromise.then((unlisten) => unlisten()).catch(() => {});
    };
  }, []);

  async function sendMessage(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();

    const text = draft.trim();
    if (!text || isSending) {
      return;
    }

    setDraft("");
    setError(null);
    setIsSending(true);
    setStatus("Curator, Id, and Ego are processing locally");

    try {
      const ack = await invoke<ChatAck>("send_chat_message", { text });
      // Append the user message immediately. The orion reply arrives later
      // via the `orion://ego/action` event listener registered above.
      setHistory((current) => [
        ...current,
        {
          id: `${ack.correlationId}-user`,
          role: "user",
          text,
          topic: "mentor.input",
          correlationId: ack.correlationId
        }
      ]);
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
      setStatus("Local round-trip failed");
      setIsSending(false);
    }
  }

  async function refreshSaoPolicy() {
    setError(null);
    setIsSyncing(true);
    setSyncStatus("Refreshing SAO policy");
    try {
      const version = await invoke<number>("refresh_sao_policy", { rules: [] });
      setCompanionStatus((current) => ({
        ...current,
        policyVersion: version
      }));
      setSyncStatus(`Policy refreshed to v${version}`);
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
      setSyncStatus("Policy refresh failed");
    } finally {
      setIsSyncing(false);
    }
  }

  async function shipSaoEgress() {
    setError(null);
    setIsSyncing(true);
    setSyncStatus("Shipping SAO egress");
    try {
      const report = await invoke<ShipReport>("ship_sao_egress");
      setLastShipReport(report);
      setCompanionStatus((current) => ({
        ...current,
        saoBacklog: Math.max(0, current.saoBacklog - report.acked)
      }));
      setSyncStatus(
        `Egress shipped: ${report.acked}/${report.attempted} acked, ${report.failed} failed`
      );
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
      setSyncStatus("Egress ship failed");
    } finally {
      setIsSyncing(false);
    }
  }

  return (
    <main className="shell">
      <section className="hero">
        <div>
          <p className="eyebrow">Phoenix Project</p>
          <h1>OrionII</h1>
          <p className="lede">
            Local-first companion runtime with durable identity, an entity-internal
            event bus, and asynchronous SAO accountability over a sanitized seam.
          </p>
        </div>
        <div className="status-card">
          <span>Status</span>
          <strong>{status}</strong>
          <dl>
            <div>
              <dt>Identity</dt>
              <dd>{companionStatus.companionId}</dd>
            </div>
            <div>
              <dt>SAO backlog</dt>
              <dd>{companionStatus.saoBacklog}</dd>
            </div>
            <div>
              <dt>Policy</dt>
              <dd>v{companionStatus.policyVersion}</dd>
            </div>
            <div>
              <dt>Memory</dt>
              <dd>{companionStatus.memoryCount} records</dd>
            </div>
            <div>
              <dt>Security</dt>
              <dd>{companionStatus.security.constitutionalIntegrity}</dd>
            </div>
            <div>
              <dt>Model</dt>
              <dd>{modelSummary(companionStatus.modelStatus)}</dd>
            </div>
            <div>
              <dt>SAO</dt>
              <dd>{saoConnection.birthed ? "birthed" : saoConnection.configured ? "anchor only" : "offline"}</dd>
            </div>
            <div>
              <dt>Bus</dt>
              <dd>{saoConnection.busTransport}</dd>
            </div>
          </dl>
        </div>
      </section>

      <section className="sync-panel" aria-label="SAO sync">
        <div>
          <p className="eyebrow">SAO sync</p>
          <strong>{syncStatus}</strong>
          {saoConnection.birthed ? (
            <p>
              Owner {saoConnection.ownerUsername ?? "(unknown)"} · provider{" "}
              <code>{saoConnection.provider}</code> · id <code>{saoConnection.idModel}</code> ·
              ego <code>{saoConnection.egoModel}</code>
              {saoConnection.birthedAt ? ` · birthed ${saoConnection.birthedAt}` : ""}
            </p>
          ) : (
            <p>
              {saoConnection.configured
                ? `Anchor target: ${saoConnection.baseUrl}${saoConnection.agentId ? `, agent ${saoConnection.agentId}` : ""}`
                : "Orion remains local-first until SAO environment variables are configured."}
            </p>
          )}
          {lastShipReport ? (
            <p>
              Last ship report: {lastShipReport.attempted} attempted, {lastShipReport.acked} acked,{" "}
              {lastShipReport.failed} failed.
            </p>
          ) : null}
        </div>
        <div className="sync-actions">
          <button type="button" onClick={refreshSaoPolicy} disabled={isSyncing}>
            Refresh policy
          </button>
          <button type="button" onClick={shipSaoEgress} disabled={isSyncing}>
            Ship egress
          </button>
        </div>
      </section>

      {!saoConnection.birthed ? (
        <section className="enroll-panel" aria-label="Enroll with SAO">
          <p className="eyebrow">SAO enrollment</p>
          <h2>No JSON paste required</h2>
          {saoConnection.configured ? (
            <p>
              OrionII found an enrollment anchor for agent{" "}
              <code>{saoConnection.agentId ?? "(unknown)"}</code>, but SAO did not
              birth it. Download the agent bundle again from SAO and run{" "}
              <code>Install-OrionII.cmd</code>. If you accidentally run{" "}
              <code>OrionII-Setup.msi</code> directly from the extracted bundle,
              the installer will also pick up the sibling <code>config.json</code>.
            </p>
          ) : (
            <p>
              Create or download an agent in SAO, extract the bundle, and
              double-click <code>Install-OrionII.cmd</code>. The installer copies
              the enrollment config automatically and OrionII will use the SAO
              container it came from.
            </p>
          )}
          {saoConnection.birthError ? (
            <p className="enroll-feedback">
              SAO response: <code>{saoConnection.birthError}</code>
            </p>
          ) : null}
        </section>
      ) : null}

      <section className="chat-panel" aria-label="Orion chat">
        <div className="transcript">
          {history.length === 0 ? (
            <div className="empty-state">
              <span>Local-first companion shell</span>
              <p>Send a message to exercise durable identity, Id, Curator, and Ego.</p>
            </div>
          ) : (
            history.map((message) => (
              <article className={`bubble ${message.role}`} key={message.id}>
                <div className="bubble-meta">
                  <span>{message.role === "user" ? "You" : "Orion"}</span>
                  <code>{message.topic}</code>
                </div>
                <p>{message.text}</p>
                <small>correlation {message.correlationId}</small>
              </article>
            ))
          )}
        </div>

        {error ? <div className="error">Error: {error}</div> : null}

        <form className="composer" onSubmit={sendMessage}>
          <input
            aria-label="Message Orion"
            placeholder="Ask Orion anything..."
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            disabled={isSending}
          />
          <button type="submit" disabled={!canSend}>
            {isSending ? "Sending" : "Send"}
          </button>
        </form>
      </section>
    </main>
  );
}

function modelSummary(statuses: ModelStatus[]): string {
  if (statuses.length === 0) {
    return "not checked";
  }

  if (statuses.some((status) => status.state === "healthy")) {
    return "Ollama";
  }

  if (statuses.some((status) => status.state === "degraded")) {
    return "Degraded fallback";
  }

  return "Fallback";
}

export default App;
