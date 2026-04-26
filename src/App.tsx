import { FormEvent, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type Payload =
  | { type: "userInput"; data: { text: string } }
  | { type: "chatOutput"; data: { text: string } }
  | { type: string; data?: unknown };

type Message = {
  id: string;
  correlationId: string;
  parentMsgId: string | null;
  kind: string;
  author: string | { agent: string };
  topic: string;
  timestamp: string;
  ttlCycles: number;
  ttlMax: number;
  priority: string;
  sessionId: string;
  payload: Payload;
};

type ChatExchange = {
  input: Message;
  idSignal: Message;
  instruction: Message;
  output: Message;
  persistedMessages: number;
  companionId: string;
  saoBacklog: number;
  policyVersion: number;
  memoryCount: number;
  security: {
    constitutionalIntegrity: string;
    checkedAt: string;
    remediation: string | null;
  };
  modelStatus: Array<{
    role: string;
    provider: string;
    state: string;
    model: string;
    message: string | null;
  }>;
};

type TranscriptMessage = {
  id: string;
  role: "user" | "orion";
  text: string;
  topic: string;
  correlationId: string;
};

type CompanionStatus = {
  companionId: string;
  saoBacklog: number;
  policyVersion: number;
  memoryCount: number;
  security: ChatExchange["security"];
  modelStatus: ChatExchange["modelStatus"];
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
};

function payloadText(message: Message): string {
  const data = message.payload.data;

  if (
    (message.payload.type === "userInput" || message.payload.type === "chatOutput") &&
    typeof data === "object" &&
    data !== null &&
    "text" in data &&
    typeof data.text === "string"
  ) {
    return data.text;
  }

  return "";
}

function App() {
  const [draft, setDraft] = useState("");
  const [history, setHistory] = useState<TranscriptMessage[]>([]);
  const [status, setStatus] = useState("M0 local bus ready");
  const [companionStatus, setCompanionStatus] = useState<CompanionStatus>({
    companionId: "not loaded",
    saoBacklog: 0,
    policyVersion: 1,
    memoryCount: 0,
    security: {
      constitutionalIntegrity: "notChecked",
      checkedAt: "",
      remediation: null
    },
    modelStatus: []
  });
  const [error, setError] = useState<string | null>(null);
  const [isSending, setIsSending] = useState(false);
  const [isSyncing, setIsSyncing] = useState(false);
  const [saoConnection, setSaoConnection] = useState<SaoConnectionStatus>({
    configured: false,
    baseUrl: null,
    agentId: null
  });
  const [syncStatus, setSyncStatus] = useState("SAO sync not checked");
  const [lastShipReport, setLastShipReport] = useState<ShipReport | null>(null);

  const canSend = useMemo(() => draft.trim().length > 0 && !isSending, [draft, isSending]);

  useEffect(() => {
    invoke<SaoConnectionStatus>("sao_connection_status")
      .then((connection) => {
        setSaoConnection(connection);
        setSyncStatus(
          connection.configured
            ? `SAO configured at ${connection.baseUrl}`
            : "Offline local mode; set SAO_BASE_URL and SAO_DEV_BEARER_TOKEN"
        );
      })
      .catch((cause) => setSyncStatus(`SAO status unavailable: ${String(cause)}`));
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
      const exchange = await invoke<ChatExchange>("send_chat_message", { text });
      setHistory((current) => [
        ...current,
        {
          id: exchange.input.id,
          role: "user",
          text: payloadText(exchange.input),
          topic: exchange.input.topic,
          correlationId: exchange.input.correlationId
        },
        {
          id: exchange.output.id,
          role: "orion",
          text: payloadText(exchange.output),
          topic: exchange.output.topic,
          correlationId: exchange.output.correlationId
        }
      ]);
      setCompanionStatus({
        companionId: exchange.companionId,
        saoBacklog: exchange.saoBacklog,
        policyVersion: exchange.policyVersion,
        memoryCount: exchange.memoryCount,
        security: exchange.security
        ,
        modelStatus: exchange.modelStatus
      });
      setStatus(`${exchange.persistedMessages} durable messages; SAO backlog ${exchange.saoBacklog}`);
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
      setStatus("Local round-trip failed");
    } finally {
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
            Local-first companion runtime with durable identity, bicameral message
            boundaries, and asynchronous SAO accountability.
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
              <dd>{saoConnection.configured ? "configured" : "offline"}</dd>
            </div>
          </dl>
        </div>
      </section>

      <section className="sync-panel" aria-label="SAO sync">
        <div>
          <p className="eyebrow">SAO sync</p>
          <strong>{syncStatus}</strong>
          <p>
            {saoConnection.configured
              ? `Connected target: ${saoConnection.baseUrl}${saoConnection.agentId ? `, agent ${saoConnection.agentId}` : ""}`
              : "Orion remains local-first until SAO environment variables are configured."}
          </p>
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

function modelSummary(statuses: ChatExchange["modelStatus"]): string {
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
