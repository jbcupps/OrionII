import { FormEvent, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  Activity,
  AlertTriangle,
  Bot,
  BrainCircuit,
  CheckCircle2,
  CloudUpload,
  Database,
  Fingerprint,
  RefreshCw,
  Route,
  Send,
  ShieldCheck,
  Wifi,
  WifiOff,
  type LucideIcon
} from "lucide-react";

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
  role: "user" | "agent";
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

type ConnectionMode = "birthed" | "anchor" | "offline";

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

const EMPTY_CONNECTION: SaoConnectionStatus = {
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
};

const ORION_EGO_ACTION_EVENT = "orion://ego/action";

function App() {
  const [draft, setDraft] = useState("");
  const [history, setHistory] = useState<TranscriptMessage[]>([]);
  const [status, setStatus] = useState("Entity bus ready");
  const [companionStatus, setCompanionStatus] =
    useState<CompanionStatusReport>(EMPTY_STATUS);
  const [error, setError] = useState<string | null>(null);
  const [isSending, setIsSending] = useState(false);
  const [isSyncing, setIsSyncing] = useState(false);
  const [saoConnection, setSaoConnection] =
    useState<SaoConnectionStatus>(EMPTY_CONNECTION);
  const [syncStatus, setSyncStatus] = useState("SAO sync not checked");
  const [lastShipReport, setLastShipReport] = useState<ShipReport | null>(null);

  const displayAgentTitle = useMemo(
    () => getDisplayAgentTitle(saoConnection),
    [saoConnection]
  );
  const connectionMode = getConnectionMode(saoConnection);
  const canSend = useMemo(
    () => draft.trim().length > 0 && !isSending,
    [draft, isSending]
  );

  useEffect(() => {
    invoke<SaoConnectionStatus>("sao_connection_status")
      .then((connection) => {
        setSaoConnection(connection);
        setSyncStatus(describeConnectionStatus(connection));
      })
      .catch((cause) => {
        setSyncStatus("SAO status unavailable");
        setError(String(cause));
      });

    invoke<CompanionStatusReport>("companion_status")
      .then(setCompanionStatus)
      .catch(() => {
        // Non-fatal: status will populate after the first ego.action event.
      });
  }, []);

  useEffect(() => {
    document.title = displayAgentTitle;
    try {
      void getCurrentWindow().setTitle(displayAgentTitle).catch(() => {});
    } catch {
      // Browser preview does not expose the Tauri window API.
    }
  }, [displayAgentTitle]);

  // Subscribe to `orion://ego/action` once at mount. This is the architectural
  // inversion: chat output flows through the bus, not through a command return.
  useEffect(() => {
    const unlistenPromise = listen<EgoActionEvent>(ORION_EGO_ACTION_EVENT, (event) => {
      const payload = event.payload;
      setHistory((current) => [
        ...current,
        {
          id: `${payload.correlationId ?? crypto.randomUUID()}-agent`,
          role: "agent",
          text: payload.responseText,
          topic: "ego.action",
          correlationId: payload.correlationId ?? "(unlinked)"
        }
      ]);
      setIsSending(false);
      setStatus("Ego action received");

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
      // Append the user message immediately. The agent reply arrives later
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
      <AppHeader
        connection={saoConnection}
        displayAgentTitle={displayAgentTitle}
        status={status}
      />

      <HealthStrip
        companionStatus={companionStatus}
        connection={saoConnection}
      />

      <div className="cockpit">
        <ChatPanel
          agentTitle={displayAgentTitle}
          canSend={canSend}
          draft={draft}
          error={error}
          history={history}
          isSending={isSending}
          mode={connectionMode}
          onDraftChange={setDraft}
          onSend={sendMessage}
        />

        <aside className="side-rail" aria-label="Agent operations">
          <ConnectionSummary
            connection={saoConnection}
            isSyncing={isSyncing}
            lastShipReport={lastShipReport}
            onRefreshPolicy={refreshSaoPolicy}
            onShipEgress={shipSaoEgress}
            syncStatus={syncStatus}
          />

          {connectionMode !== "birthed" ? (
            <EnrollmentNotice connection={saoConnection} />
          ) : null}

          <DiagnosticsPanel
            companionStatus={companionStatus}
            connection={saoConnection}
          />
        </aside>
      </div>
    </main>
  );
}

function AppHeader({
  connection,
  displayAgentTitle,
  status
}: {
  connection: SaoConnectionStatus;
  displayAgentTitle: string;
  status: string;
}) {
  const mode = getConnectionMode(connection);
  const modeLabel = getConnectionLabel(mode);

  return (
    <header className="app-header">
      <div className="agent-mark" aria-hidden="true">
        <Bot size={24} />
      </div>
      <div className="agent-title">
        <span>{modeLabel}</span>
        <h1>{displayAgentTitle}</h1>
      </div>
      <div className={`connection-pill ${mode}`}>
        {mode === "offline" ? <WifiOff size={16} /> : <Wifi size={16} />}
        <span>{status}</span>
      </div>
    </header>
  );
}

function HealthStrip({
  companionStatus,
  connection
}: {
  companionStatus: CompanionStatusReport;
  connection: SaoConnectionStatus;
}) {
  const mode = getConnectionMode(connection);
  const policyVersion = connection.policyVersion ?? companionStatus.policyVersion;

  return (
    <section className="health-strip" aria-label="Agent health">
      <HealthChip
        icon={Activity}
        label="SAO"
        tone={mode === "birthed" ? "good" : mode === "anchor" ? "warn" : "muted"}
        value={getConnectionLabel(mode)}
      />
      <HealthChip icon={ShieldCheck} label="Policy" value={`v${policyVersion}`} />
      <HealthChip
        icon={CloudUpload}
        label="Backlog"
        tone={companionStatus.saoBacklog > 0 ? "warn" : "good"}
        value={String(companionStatus.saoBacklog)}
      />
      <HealthChip
        icon={BrainCircuit}
        label="Model"
        tone={getModelTone(companionStatus.modelStatus)}
        value={formatModelSummary(companionStatus.modelStatus)}
      />
      <HealthChip
        icon={Route}
        label="Bus"
        value={formatBusLabel(connection.busTransport)}
      />
      <HealthChip
        icon={Database}
        label="Memory"
        value={`${companionStatus.memoryCount} records`}
      />
      <HealthChip
        icon={ShieldCheck}
        label="Security"
        tone={getSecurityTone(companionStatus.security.constitutionalIntegrity)}
        value={formatStatusText(companionStatus.security.constitutionalIntegrity)}
      />
    </section>
  );
}

function HealthChip({
  icon: Icon,
  label,
  tone = "neutral",
  value
}: {
  icon: LucideIcon;
  label: string;
  tone?: "good" | "warn" | "muted" | "neutral";
  value: string;
}) {
  return (
    <div className={`health-chip ${tone}`}>
      <Icon size={16} aria-hidden="true" />
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function ChatPanel({
  agentTitle,
  canSend,
  draft,
  error,
  history,
  isSending,
  mode,
  onDraftChange,
  onSend
}: {
  agentTitle: string;
  canSend: boolean;
  draft: string;
  error: string | null;
  history: TranscriptMessage[];
  isSending: boolean;
  mode: ConnectionMode;
  onDraftChange: (value: string) => void;
  onSend: (event: FormEvent<HTMLFormElement>) => void;
}) {
  return (
    <section className="chat-panel" aria-label={`Chat with ${agentTitle}`}>
      <div className="chat-heading">
        <div>
          <span>Conversation</span>
          <h2>{agentTitle}</h2>
        </div>
        {isSending ? <div className="pending-dot">Thinking</div> : null}
      </div>

      <div className="transcript">
        {history.length === 0 ? (
          <EmptyState agentTitle={agentTitle} mode={mode} />
        ) : (
          history.map((message) => (
            <article className={`bubble ${message.role}`} key={message.id}>
              <div className="bubble-meta">
                <span>{message.role === "user" ? "You" : agentTitle}</span>
                <code>{message.topic}</code>
              </div>
              <p>{message.text}</p>
              <small>correlation {message.correlationId}</small>
            </article>
          ))
        )}
      </div>

      {error ? <InlineAlert detail={error} /> : null}

      <form className="composer" onSubmit={onSend}>
        <input
          aria-label={`Message ${agentTitle}`}
          placeholder={`Message ${agentTitle}...`}
          value={draft}
          onChange={(event) => onDraftChange(event.target.value)}
          disabled={isSending}
        />
        <button type="submit" disabled={!canSend} title="Send message">
          {isSending ? <RefreshCw size={18} aria-hidden="true" /> : <Send size={18} aria-hidden="true" />}
          <span>{isSending ? "Sending" : "Send"}</span>
        </button>
      </form>
    </section>
  );
}

function EmptyState({
  agentTitle,
  mode
}: {
  agentTitle: string;
  mode: ConnectionMode;
}) {
  if (mode === "birthed") {
    return (
      <div className="empty-state">
        <CheckCircle2 size={34} aria-hidden="true" />
        <span>Start talking to {agentTitle}</span>
        <p>The agent is connected, governed, and ready for a local-first session.</p>
      </div>
    );
  }

  return (
    <div className="empty-state">
      <AlertTriangle size={34} aria-hidden="true" />
      <span>{mode === "anchor" ? "Enrollment needs attention" : "Enrollment not found"}</span>
      <p>
        {mode === "anchor"
          ? "This install has a SAO anchor, but birth has not completed yet."
          : "Download an agent bundle from SAO and run the installer script to connect this runtime."}
      </p>
    </div>
  );
}

function InlineAlert({ detail }: { detail: string }) {
  return (
    <div className="inline-alert" role="alert">
      <AlertTriangle size={18} aria-hidden="true" />
      <div>
        <strong>Something needs attention</strong>
        <p>The command failed, but the local runtime stayed available.</p>
        <details>
          <summary>Technical detail</summary>
          <code>{detail}</code>
        </details>
      </div>
    </div>
  );
}

function ConnectionSummary({
  connection,
  isSyncing,
  lastShipReport,
  onRefreshPolicy,
  onShipEgress,
  syncStatus
}: {
  connection: SaoConnectionStatus;
  isSyncing: boolean;
  lastShipReport: ShipReport | null;
  onRefreshPolicy: () => void;
  onShipEgress: () => void;
  syncStatus: string;
}) {
  return (
    <section className="panel connection-summary" aria-label="SAO sync">
      <div className="panel-heading">
        <div>
          <span>SAO sync</span>
          <h2>{syncStatus}</h2>
        </div>
        {connection.birthed ? (
          <CheckCircle2 size={20} aria-label="Connected" />
        ) : (
          <AlertTriangle size={20} aria-label="Needs attention" />
        )}
      </div>

      <p className="summary-copy">{getConnectionDetail(connection)}</p>

      {lastShipReport ? (
        <p className="ship-report">
          Last ship: {lastShipReport.attempted} attempted, {lastShipReport.acked} acked,
          {" "}
          {lastShipReport.failed} failed.
        </p>
      ) : null}

      <div className="action-row">
        <button type="button" onClick={onRefreshPolicy} disabled={isSyncing}>
          <RefreshCw size={16} aria-hidden="true" />
          <span>Refresh policy</span>
        </button>
        <button type="button" onClick={onShipEgress} disabled={isSyncing}>
          <CloudUpload size={16} aria-hidden="true" />
          <span>Ship egress</span>
        </button>
      </div>
    </section>
  );
}

function EnrollmentNotice({ connection }: { connection: SaoConnectionStatus }) {
  const configured = connection.configured;

  return (
    <section className="panel enrollment-notice" aria-label="SAO enrollment">
      <div className="panel-heading">
        <div>
          <span>Enrollment</span>
          <h2>{configured ? "Anchor found, birth incomplete" : "Connect an agent bundle"}</h2>
        </div>
        <Fingerprint size={20} aria-hidden="true" />
      </div>
      {configured ? (
        <p>
          This runtime found an enrollment anchor for agent{" "}
          <code>{shortId(connection.agentId)}</code>, but SAO did not complete birth. Download
          a fresh agent bundle from SAO and run <code>Install-OrionII.cmd</code> from the
          extracted bundle so <code>config.json</code> is copied automatically.
        </p>
      ) : (
        <p>
          Create or download an agent in SAO, extract the bundle, and run{" "}
          <code>Install-OrionII.cmd</code>. The installer copies the enrollment config and
          this runtime will use the SAO container it came from.
        </p>
      )}
      {connection.birthError ? (
        <details className="birth-error">
          <summary>SAO response</summary>
          <code>{connection.birthError}</code>
        </details>
      ) : null}
    </section>
  );
}

function DiagnosticsPanel({
  companionStatus,
  connection
}: {
  companionStatus: CompanionStatusReport;
  connection: SaoConnectionStatus;
}) {
  return (
    <section className="panel diagnostics-panel" aria-label="Diagnostics">
      <div className="panel-heading">
        <div>
          <span>Diagnostics</span>
          <h2>Runtime context</h2>
        </div>
        <Activity size={20} aria-hidden="true" />
      </div>

      <dl className="detail-list">
        <DetailRow label="Owner" value={connection.ownerUsername ?? "not available"} />
        <DetailRow label="Agent id" value={connection.agentId ?? "not assigned"} />
        <DetailRow label="Identity" value={companionStatus.companionId} />
        <DetailRow label="Provider" value={connection.provider ?? "fallback"} />
        <DetailRow label="Id model" value={connection.idModel ?? "fallback"} />
        <DetailRow label="Ego model" value={connection.egoModel ?? "fallback"} />
        <DetailRow label="Birthed" value={formatTimestamp(connection.birthedAt)} />
        <DetailRow label="Messages" value={String(companionStatus.persistedMessages)} />
        <DetailRow
          label="Security check"
          value={formatTimestamp(companionStatus.security.checkedAt)}
        />
        {companionStatus.security.remediation ? (
          <DetailRow label="Remediation" value={companionStatus.security.remediation} />
        ) : null}
      </dl>
    </section>
  );
}

function DetailRow({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt>{label}</dt>
      <dd title={value}>{value}</dd>
    </div>
  );
}

function getDisplayAgentTitle(connection: SaoConnectionStatus): string {
  if (connection.birthed && connection.agentName?.trim()) {
    return connection.agentName.trim();
  }

  if (connection.configured && connection.agentId) {
    return `Agent ${shortId(connection.agentId)}`;
  }

  return "Unenrolled agent";
}

function getConnectionMode(connection: SaoConnectionStatus): ConnectionMode {
  if (connection.birthed) {
    return "birthed";
  }

  if (connection.configured) {
    return "anchor";
  }

  return "offline";
}

function getConnectionLabel(mode: ConnectionMode): string {
  if (mode === "birthed") {
    return "Birthed";
  }

  if (mode === "anchor") {
    return "Anchor only";
  }

  return "Offline";
}

function describeConnectionStatus(connection: SaoConnectionStatus): string {
  if (connection.birthed) {
    return `Birthed via ${connection.provider ?? "fallback provider"} (policy v${
      connection.policyVersion ?? 0
    })`;
  }

  if (connection.configured) {
    return connection.birthError
      ? "Enrollment anchor loaded; SAO rejected birth"
      : "Enrollment anchor loaded; SAO is unreachable";
  }

  return "Offline local mode; install from a SAO agent bundle to enroll";
}

function getConnectionDetail(connection: SaoConnectionStatus): string {
  if (connection.birthed) {
    return `Owner ${connection.ownerUsername ?? "unknown"}; provider ${
      connection.provider ?? "fallback"
    }; id ${connection.idModel ?? "fallback"}; ego ${connection.egoModel ?? "fallback"}.`;
  }

  if (connection.configured) {
    return `Anchor target: ${connection.baseUrl ?? "unknown"}; agent ${shortId(
      connection.agentId
    )}.`;
  }

  return "No SAO anchor is configured yet. The runtime remains local-first until enrollment.";
}

function shortId(id: string | null): string {
  if (!id) {
    return "unknown";
  }

  if (id.length <= 12) {
    return id;
  }

  return `${id.slice(0, 8)}...${id.slice(-4)}`;
}

function formatModelSummary(statuses: ModelStatus[]): string {
  if (statuses.length === 0) {
    return "not checked";
  }

  const healthy = statuses.find((status) => status.state === "healthy");
  if (healthy) {
    return healthy.provider || healthy.model || "healthy";
  }

  const degraded = statuses.find((status) => status.state === "degraded");
  if (degraded) {
    return "Degraded fallback";
  }

  return "Fallback";
}

function getModelTone(statuses: ModelStatus[]): "good" | "warn" | "muted" | "neutral" {
  if (statuses.some((status) => status.state === "healthy")) {
    return "good";
  }

  if (statuses.some((status) => status.state === "degraded")) {
    return "warn";
  }

  return statuses.length === 0 ? "muted" : "neutral";
}

function getSecurityTone(value: string): "good" | "warn" | "muted" | "neutral" {
  const normalized = value.toLowerCase();
  if (normalized.includes("healthy") || normalized.includes("ok") || normalized.includes("valid")) {
    return "good";
  }

  if (normalized.includes("fail") || normalized.includes("invalid")) {
    return "warn";
  }

  return normalized.includes("not") ? "muted" : "neutral";
}

function formatStatusText(value: string): string {
  if (!value) {
    return "unknown";
  }

  return value
    .replace(/([a-z])([A-Z])/g, "$1 $2")
    .replace(/_/g, " ")
    .toLowerCase();
}

function formatBusLabel(value: string): string {
  return value.replace(/_/g, " ");
}

function formatTimestamp(value: string | null): string {
  if (!value) {
    return "not available";
  }

  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) {
    return value;
  }

  return parsed.toLocaleString();
}

export default App;
