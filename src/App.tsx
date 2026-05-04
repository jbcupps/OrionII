import { FormEvent, useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

import Commissioning, { CommissioningStateView } from "./Commissioning";
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
// reply arrives asynchronously on the `orion://ego/action` Tauri event
// (see shared const `orion::EGO_ACTION_EVENT` re-exported from service.rs
// via orion/mod.rs; the TS const ORION_EGO_ACTION_EVENT must match exactly).
// Emitted by the UI emitter subscriber on `Topic::EgoAction`.

type ChatAck = {
  correlationId: string;
  accepted: boolean;
};

type EgoActionEvent = {
  correlationId: string | null;
  userQuery: string;
  responseText: string;
  status: string;
  error: string | null;
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
  lastEgoActionAt: string | null;
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
  agentNameSource: "birth" | "bundle" | "tokenClaim" | "none";
  ownerUsername: string | null;
  provider: string | null;
  idModel: string | null;
  egoModel: string | null;
  birthedAt: string | null;
  policyVersion: number | null;
  birthError: string | null;
  birthStatusCode: number | null;
  busTransport: string;
};

type ConnectionMode = "birthed" | "anchor" | "offline";

type ApplyConfigResult = {
  writtenTo: string;
  status: SaoConnectionStatus;
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
  modelStatus: [],
  lastEgoActionAt: null
};

const EMPTY_CONNECTION: SaoConnectionStatus = {
  configured: false,
  baseUrl: null,
  agentId: null,
  birthed: false,
  agentName: null,
  agentNameSource: "none",
  ownerUsername: null,
  provider: null,
  idModel: null,
  egoModel: null,
  birthedAt: null,
  policyVersion: null,
  birthError: null,
  birthStatusCode: null,
  busTransport: "in_memory"
};

const ORION_EGO_ACTION_EVENT = "orion://ego/action";

/// Upper bound on how long the chat surface waits for a reply on
/// `orion://ego/action` before flipping `isSending` off and surfacing an
/// error. Slightly longer than the Rust-side `EGO_MODEL_CALL_TIMEOUT` so
/// the degraded fallback usually wins the race; if neither arrives the
/// operator at least sees something instead of a silent infinite spinner.
const ORION_REPLY_TIMEOUT_MS = 35_000;

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
  const [bundleJson, setBundleJson] = useState("");
  const [bundleApplyStatus, setBundleApplyStatus] = useState<string | null>(null);
  const [isApplyingBundle, setIsApplyingBundle] = useState(false);
  const [pendingCorrelationId, setPendingCorrelationId] = useState<string | null>(null);
  const [commissioningView, setCommissioningView] =
    useState<CommissioningStateView | null>(null);
  const [isCheckingEnrollment, setIsCheckingEnrollment] = useState(true);
  const [commissioningError, setCommissioningError] = useState<string | null>(null);

  const displayAgentTitle = useMemo(
    () => getDisplayAgentTitle(saoConnection),
    [saoConnection]
  );
  const connectionMode = getConnectionMode(saoConnection);
  const activeCommissioningView = useMemo(
    () => getActiveCommissioningView(saoConnection, commissioningView),
    [saoConnection, commissioningView]
  );
  const canSend = useMemo(
    () => draft.trim().length > 0 && !isSending,
    [draft, isSending]
  );

  const refreshCompanionStatus = useCallback(async () => {
    return invoke<CompanionStatusReport>("companion_status")
      .then(setCompanionStatus)
      .catch(() => {
        // Non-fatal: status will populate after the first ego.action event.
      });
  }, []);

  const refreshEnrollment = useCallback(async () => {
    setIsCheckingEnrollment(true);
    setCommissioningError(null);

    try {
      const connection = await invoke<SaoConnectionStatus>("sao_connection_status");
      setSaoConnection(connection);
      setSyncStatus(describeConnectionStatus(connection));
    } catch (cause) {
      const message = formatError(cause);
      setSyncStatus("SAO status unavailable");
      setError(message);
    }

    try {
      const view = await invoke<CommissioningStateView>("commissioning_state");
      setCommissioningView(view);
    } catch (cause) {
      setCommissioningView(null);
      setCommissioningError(formatError(cause));
    } finally {
      setIsCheckingEnrollment(false);
    }
  }, []);

  useEffect(() => {
    void refreshEnrollment();
    void refreshCompanionStatus();
  }, [refreshEnrollment, refreshCompanionStatus]);

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
  // Improved with strict correlation matching, dedup, status-based UX (degraded
  // errors now surface), and Markdown rendering for agent responses.
  useEffect(() => {
    const unlistenPromise = listen<EgoActionEvent>(ORION_EGO_ACTION_EVENT, (event) => {
      const payload = event.payload;
      const correlationId = payload.correlationId;

      setHistory((current) => {
        // Dedup by correlationId to prevent duplicate bubbles from out-of-order
        // or replayed EgoActions (common in NATS/JetStream durable mode).
        const last = current[current.length - 1];
        if (last && last.correlationId === correlationId && last.role === "agent") {
          return current;
        }
        return [
          ...current,
          {
            id: `${correlationId ?? crypto.randomUUID()}-agent`,
            role: "agent",
            text: payload.responseText,
            topic: "ego.action",
            correlationId: correlationId ?? "(unlinked)"
          }
        ];
      });

      // Strict matching: only clear sending/pending for the expected correlation.
      // This fixes races with concurrent sends or out-of-order events.
      if (correlationId) {
        setPendingCorrelationId((current) =>
          current === correlationId ? null : current
        );
        if (payload.status !== "success" && payload.error) {
          setError(payload.error);
          setStatus(`Degraded response (${payload.status})`);
        } else {
          setStatus("Ego action received");
        }
        setIsSending(false);
      } else {
        setIsSending(false);
      }

      // Refresh persistence-derived status (journal, lastEgoActionAt, etc.)
      // after each ego response.
      invoke<CompanionStatusReport>("companion_status")
        .then(setCompanionStatus)
        .catch(() => {
          // Non-fatal.
        });
      window.setTimeout(() => {
        invoke<CompanionStatusReport>("companion_status")
          .then(setCompanionStatus)
          .catch(() => {});
      }, 100);
    });

    return () => {
      unlistenPromise.then((unlisten) => unlisten()).catch(() => {});
    };
  }, []);

  // Watchdog: if a sent message has no `orion://ego/action` event within
  // ORION_REPLY_TIMEOUT_MS, surface the failure instead of leaving the
  // spinner up forever. The Rust side's EGO_MODEL_CALL_TIMEOUT (30s) wraps
  // the model call so a degraded fallback should arrive within ~30s; this
  // 35s timer is a hard floor for the operator UX.
  useEffect(() => {
    if (!pendingCorrelationId) {
      return;
    }
    const handle = window.setTimeout(() => {
      setError(
        `No agent reply within ${Math.round(ORION_REPLY_TIMEOUT_MS / 1000)}s for correlation ${pendingCorrelationId}. The model layer or bus may be wedged — check the diagnostics panel and Tauri logs.`
      );
      setStatus("Reply watchdog tripped");
      setIsSending(false);
      setPendingCorrelationId(null);
    }, ORION_REPLY_TIMEOUT_MS);

    return () => window.clearTimeout(handle);
  }, [pendingCorrelationId]);

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
      setPendingCorrelationId(ack.correlationId);
    } catch (cause) {
      const message = formatError(cause);
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
      const message = formatError(cause);
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
      const message = formatError(cause);
      setError(message);
      setSyncStatus("Egress ship failed");
    } finally {
      setIsSyncing(false);
    }
  }

  async function applyBundleConfig() {
    if (!bundleJson.trim() || isApplyingBundle) {
      return;
    }

    setError(null);
    setBundleApplyStatus("Applying enrollment config");
    setIsApplyingBundle(true);
    try {
      const result = await invoke<ApplyConfigResult>("apply_bundle_config", {
        json: bundleJson
      });
      setSaoConnection(result.status);
      setSyncStatus(describeConnectionStatus(result.status));
      setBundleApplyStatus(`Config saved to ${result.writtenTo}`);
      setBundleJson("");
      await refreshEnrollment();
      await refreshCompanionStatus();
    } catch (cause) {
      const message = formatError(cause);
      setError(message);
      setBundleApplyStatus("Config apply failed");
    } finally {
      setIsApplyingBundle(false);
    }
  }

  // Commissioning gate: when the SAO bundle is configured but the agent
  // has not been commissioned yet (or local state needs repair), show the
  // commissioning surface instead of the cockpit. Once commissioning
  // completes, the hot-swap inside `commission_finalize` flips
  // birth.is_some()=true and `commissioning_state` reports
  // "commissioned" — refreshEnrollment clears this branch and the
  // operator lands on the chat cockpit.
  if (isCheckingEnrollment) {
    return <EnrollmentGatePending />;
  }

  if (commissioningError) {
    return (
      <CommissioningUnavailable
        connection={saoConnection}
        detail={commissioningError}
        onRetry={refreshEnrollment}
      />
    );
  }

  if (activeCommissioningView) {
    return (
      <Commissioning
        initial={activeCommissioningView}
        onCommissioned={async () => {
          await refreshEnrollment();
          await refreshCompanionStatus();
        }}
      />
    );
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
            <EnrollmentNotice
              bundleJson={bundleJson}
              connection={saoConnection}
              isApplying={isApplyingBundle}
              onApply={applyBundleConfig}
              onBundleJsonChange={setBundleJson}
              status={bundleApplyStatus}
            />
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

function EnrollmentGatePending() {
  return (
    <main className="commissioning-shell">
      <header className="commissioning-header">
        <div className="commissioning-mark" aria-hidden="true">
          <RefreshCw size={22} className="commissioning-spin" />
        </div>
        <div>
          <span className="commissioning-eyebrow">OrionII enrollment</span>
          <h1>Checking agent state</h1>
        </div>
      </header>
      <section className="commissioning-panel">
        <h2>Loading enrollment</h2>
        <p className="commissioning-loading">
          <RefreshCw size={16} className="commissioning-spin" /> Reading the local
          SAO anchor and commissioning state...
        </p>
      </section>
    </main>
  );
}

function CommissioningUnavailable({
  connection,
  detail,
  onRetry
}: {
  connection: SaoConnectionStatus;
  detail: string;
  onRetry: () => Promise<void>;
}) {
  const title = getDisplayAgentTitle(connection);

  return (
    <main className="commissioning-shell">
      <header className="commissioning-header">
        <div className="commissioning-mark" aria-hidden="true">
          <AlertTriangle size={22} />
        </div>
        <div>
          <span className="commissioning-eyebrow">OrionII enrollment</span>
          <h1>Commissioning unavailable</h1>
        </div>
      </header>
      <section className="commissioning-panel">
        <h2>Runtime mismatch</h2>
        <p>
          This install found a SAO anchor for <strong>{title}</strong>, but
          the local runtime could not open the commissioning command surface.
          Install the latest OrionII bundle from SAO, then retry this check.
        </p>
        <details className="birth-error" open>
          <summary>Command error</summary>
          <code>{detail}</code>
        </details>
        <div className="commissioning-actions">
          <button
            type="button"
            className="commissioning-primary"
            onClick={() => void onRetry()}
          >
            <RefreshCw size={16} />
            <span>Retry enrollment check</span>
          </button>
        </div>
      </section>
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
        tone={getModelTone(companionStatus.modelStatus, connection)}
        value={formatModelSummary(companionStatus.modelStatus, connection)}
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
              {message.role === "agent" ? (
                <ReactMarkdown
                  remarkPlugins={[remarkGfm]}
                  className="prose prose-sm max-w-none dark:prose-invert"
                >
                  {message.text}
                </ReactMarkdown>
              ) : (
                <p>{message.text}</p>
              )}
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

function EnrollmentNotice({
  bundleJson,
  connection,
  isApplying,
  onApply,
  onBundleJsonChange,
  status
}: {
  bundleJson: string;
  connection: SaoConnectionStatus;
  isApplying: boolean;
  onApply: () => void;
  onBundleJsonChange: (value: string) => void;
  status: string | null;
}) {
  const configured = connection.configured;
  const title = getDisplayAgentTitle(connection);

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
          This runtime found an enrollment anchor for <strong>{title}</strong>{" "}
          <code>{shortId(connection.agentId)}</code>, but SAO did not complete birth.
          Download a fresh agent bundle from SAO and run <code>Install-OrionII.cmd</code>,
          or paste the bundle <code>config.json</code> here.
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
      <div className="config-apply">
        <label htmlFor="bundle-config">Bundle config</label>
        <textarea
          id="bundle-config"
          spellCheck={false}
          value={bundleJson}
          onChange={(event) => onBundleJsonChange(event.target.value)}
          placeholder='{"sao_base_url":"http://localhost:3100","agent_token":"..."}'
          disabled={isApplying}
        />
        <div className="config-apply-row">
          <button
            type="button"
            onClick={onApply}
            disabled={isApplying || !bundleJson.trim()}
          >
            {isApplying ? <RefreshCw size={16} aria-hidden="true" /> : <Fingerprint size={16} aria-hidden="true" />}
            <span>{isApplying ? "Applying" : "Apply config"}</span>
          </button>
          {status ? <small>{status}</small> : null}
        </div>
      </div>
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
        <DetailRow label="Agent name" value={connection.agentName ?? "not available"} />
        <DetailRow label="Name source" value={formatNameSource(connection.agentNameSource)} />
        <DetailRow label="Agent id" value={connection.agentId ?? "not assigned"} />
        <DetailRow label="Identity" value={companionStatus.companionId} />
        <DetailRow label="Provider" value={connection.provider ?? "fallback"} />
        <DetailRow label="Id model" value={connection.idModel ?? "fallback"} />
        <DetailRow label="Ego model" value={connection.egoModel ?? "fallback"} />
        <DetailRow label="Birthed" value={formatTimestamp(connection.birthedAt)} />
        <DetailRow
          label="Birth code"
          value={connection.birthStatusCode ? String(connection.birthStatusCode) : "not available"}
        />
        <DetailRow label="Messages" value={String(companionStatus.persistedMessages)} />
        <DetailRow
          label="Last reply"
          value={formatTimestamp(companionStatus.lastEgoActionAt)}
        />
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

function getActiveCommissioningView(
  connection: SaoConnectionStatus,
  view: CommissioningStateView | null
): CommissioningStateView | null {
  if (!connection.configured || connection.birthed) {
    return null;
  }

  if (view && view.stage !== "commissioned" && view.stage !== "notConfigured") {
    return view;
  }

  if (connection.birthStatusCode === 401 || connection.birthStatusCode === 403) {
    return { stage: "needsTokenRefresh", agentName: connection.agentName };
  }

  return { stage: "firstLaunch" };
}

function getDisplayAgentTitle(connection: SaoConnectionStatus): string {
  if (connection.agentName?.trim()) {
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
    if (connection.birthStatusCode === 401 || connection.birthStatusCode === 403) {
      return "Enrollment anchor loaded; token rejected";
    }

    if (connection.birthError) {
      return "Enrollment anchor loaded; birth failed";
    }

    return "Enrollment anchor loaded; SAO is unreachable";
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
    )}; provider ${connection.provider ?? "fallback"}; id ${
      connection.idModel ?? "fallback"
    }; ego ${connection.egoModel ?? "fallback"}.`;
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

function formatModelSummary(statuses: ModelStatus[], connection: SaoConnectionStatus): string {
  if (
    connection.configured &&
    !connection.birthed &&
    (connection.birthStatusCode === 401 || connection.birthStatusCode === 403)
  ) {
    return "Token rejected";
  }

  if (statuses.length === 0) {
    if (connection.configured && connection.provider) {
      return `${connection.provider} ready`;
    }

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

function getModelTone(
  statuses: ModelStatus[],
  connection: SaoConnectionStatus
): "good" | "warn" | "muted" | "neutral" {
  if (statuses.some((status) => status.state === "healthy")) {
    return "good";
  }

  if (
    connection.configured &&
    !connection.birthed &&
    (connection.birthStatusCode === 401 || connection.birthStatusCode === 403)
  ) {
    return "warn";
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

function formatNameSource(value: SaoConnectionStatus["agentNameSource"]): string {
  if (value === "tokenClaim") {
    return "token claim";
  }

  return value;
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

function formatError(cause: unknown): string {
  if (cause instanceof Error) {
    return cause.message;
  }

  if (typeof cause === "string") {
    return cause;
  }

  if (cause && typeof cause === "object") {
    const shaped = cause as { message?: unknown };
    if (typeof shaped.message === "string" && shaped.message.trim()) {
      return shaped.message;
    }

    try {
      return JSON.stringify(cause);
    } catch {
      return String(cause);
    }
  }

  return String(cause);
}

export default App;
