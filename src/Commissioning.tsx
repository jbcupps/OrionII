// Commissioning surface — replaces the old "Enrollment needs attention"
// notice with a staged onboarding flow. The cockpit gates on this whenever
// the SAO bundle is present but birth has not completed (or local state is
// missing). All Rust-side state lives behind the `commission_*` Tauri
// commands; this file is just the stage machine.
//
// Stages: welcome -> identityKey -> chooseRole -> defineCharterFast |
//   defineCharterQna -> reviewCharter -> register -> ready
// Repair sub-modes (refresh credentials / re-bind) sit beside the main
// flow and are entered directly when `commissioning_state` reports them.

import { useEffect, useMemo, useReducer, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  AlertTriangle,
  ArrowRight,
  Briefcase,
  CheckCircle2,
  Fingerprint,
  KeyRound,
  Loader2,
  MessageSquare,
  RefreshCw,
  Wand2
} from "lucide-react";

// --- Types mirrored from Rust ----------------------------------------

type SlotKind = "text" | "url" | "timezone" | "multiline";

type RoleSlotSummary = {
  key: string;
  label: string;
  kind: SlotKind;
};

type RoleSummary = {
  key: string;
  displayName: string;
  description: string;
  timeEstimateMinutes: number;
  slots: RoleSlotSummary[];
};

type StartResponse = {
  commissionId: string;
  mentorId: string;
  agentId: string;
  mentorPublicKeyFpr: string;
  entityPublicKeyFpr: string;
  allowedRoleKeys: string[];
  qAndAEnabled: boolean;
  saoProvider: string | null;
  saoIdModel: string | null;
  saoEgoModel: string | null;
};

type FinalizeResult = {
  soulRef: string;
  charterHash: string;
  status: unknown; // SaoConnectionStatus, opaque here
};

export type CommissioningStateView =
  | { stage: "notConfigured" }
  | { stage: "firstLaunch" }
  | { stage: "commissioned" }
  | { stage: "needsTokenRefresh"; agentName: string | null }
  | { stage: "needsRebind"; agentId: string };

type RepairStateView = Extract<
  CommissioningStateView,
  { stage: "needsTokenRefresh" } | { stage: "needsRebind" }
>;

// --- Reducer ---------------------------------------------------------

type Path = "fast" | "qna";

type Stage =
  | "welcome"
  | "identity"
  | "chooseRole"
  | "defineCharterFast"
  | "defineCharterQna"
  | "reviewCharter"
  | "register"
  | "ready"
  | "repair";

type State = {
  stage: Stage;
  startResponse?: StartResponse;
  selectedRole?: RoleSummary;
  selectedPath: Path;
  slotValues: Record<string, string>;
  qnaDescription: string;
  charterDraft: string;
  finalizeResult?: FinalizeResult;
  repairInitial?: RepairStateView;
  error?: string;
};

type Action =
  | { type: "goto"; stage: Stage }
  | { type: "start_response"; response: StartResponse }
  | { type: "select_role"; role: RoleSummary; path: Path }
  | { type: "set_slot"; key: string; value: string }
  | { type: "set_qna"; description: string }
  | { type: "set_charter"; text: string }
  | { type: "finalized"; result: FinalizeResult }
  | { type: "repair"; initial: RepairStateView }
  | { type: "error"; message: string }
  | { type: "clear_error" };

function reducer(state: State, action: Action): State {
  switch (action.type) {
    case "goto":
      return { ...state, stage: action.stage, error: undefined };
    case "start_response":
      return { ...state, startResponse: action.response };
    case "select_role":
      return {
        ...state,
        selectedRole: action.role,
        selectedPath: action.path,
        slotValues: Object.fromEntries(action.role.slots.map((s) => [s.key, ""])),
        stage: action.path === "fast" ? "defineCharterFast" : "defineCharterQna"
      };
    case "set_slot":
      return {
        ...state,
        slotValues: { ...state.slotValues, [action.key]: action.value }
      };
    case "set_qna":
      return { ...state, qnaDescription: action.description };
    case "set_charter":
      return { ...state, charterDraft: action.text };
    case "finalized":
      return { ...state, finalizeResult: action.result, stage: "ready" };
    case "repair":
      return {
        ...state,
        repairInitial: action.initial,
        stage: "repair",
        error: undefined
      };
    case "error":
      return { ...state, error: action.message };
    case "clear_error":
      return { ...state, error: undefined };
  }
}

const INITIAL_STATE: State = {
  stage: "welcome",
  selectedPath: "fast",
  slotValues: {},
  qnaDescription: "",
  charterDraft: ""
};

// --- Top-level component --------------------------------------------

export function Commissioning({
  initial,
  onCommissioned
}: {
  initial: CommissioningStateView;
  onCommissioned: () => void;
}) {
  const [state, dispatch] = useReducer(
    reducer,
    INITIAL_STATE,
    (s): State => ({ ...s, stage: initial.stage === "needsTokenRefresh" || initial.stage === "needsRebind" ? "repair" : "welcome" })
  );

  return (
    <main className="commissioning-shell">
      <header className="commissioning-header">
        <div className="commissioning-mark" aria-hidden="true">
          <Briefcase size={22} />
        </div>
        <div>
          <span className="commissioning-eyebrow">OrionII commissioning</span>
          <h1>Define and register this agent</h1>
        </div>
      </header>

      {state.error ? (
        <div className="commissioning-error" role="alert">
          <AlertTriangle size={18} />
          <span>{state.error}</span>
          <button
            type="button"
            onClick={() => dispatch({ type: "clear_error" })}
            className="commissioning-error-dismiss"
          >
            Dismiss
          </button>
        </div>
      ) : null}

      <Stage state={state} dispatch={dispatch} initial={initial} onCommissioned={onCommissioned} />
    </main>
  );
}

function Stage({
  state,
  dispatch,
  initial,
  onCommissioned
}: {
  state: State;
  dispatch: React.Dispatch<Action>;
  initial: CommissioningStateView;
  onCommissioned: () => void;
}) {
  switch (state.stage) {
    case "welcome":
      return <Welcome dispatch={dispatch} />;
    case "identity":
      return <IdentityKey state={state} dispatch={dispatch} />;
    case "chooseRole":
      return <ChooseRole dispatch={dispatch} />;
    case "defineCharterFast":
      return <DefineCharterFast state={state} dispatch={dispatch} />;
    case "defineCharterQna":
      return <DefineCharterQna state={state} dispatch={dispatch} />;
    case "reviewCharter":
      return <ReviewCharter state={state} dispatch={dispatch} />;
    case "register":
      return <RegisterWithSao state={state} dispatch={dispatch} />;
    case "ready":
      return <Ready state={state} onCommissioned={onCommissioned} />;
    case "repair":
      return <Repair initial={state.repairInitial ?? initial} dispatch={dispatch} onCommissioned={onCommissioned} />;
  }
}

// --- Stage components ------------------------------------------------

function Welcome({ dispatch }: { dispatch: React.Dispatch<Action> }) {
  return (
    <section className="commissioning-panel">
      <h2>Welcome</h2>
      <p>
        OrionII runs as a <strong>commissioned entity</strong>: every agent is
        defined by a charter that names its purpose, scope, and boundaries.
        SAO holds the cryptographic keys that identify this agent and
        counter-signs the charter so its provenance is auditable.
      </p>
      <p>
        Commissioning takes a few minutes. You'll pick a role, fill in or
        describe what you need, review the charter, and SAO will issue a
        birth certificate at the end. Nothing is registered until you
        approve the charter.
      </p>
      <div className="commissioning-actions">
        <button
          type="button"
          className="commissioning-primary"
          onClick={() => dispatch({ type: "goto", stage: "identity" })}
        >
          <span>Begin commissioning</span>
          <ArrowRight size={16} />
        </button>
      </div>
    </section>
  );
}

function IdentityKey({
  state,
  dispatch
}: {
  state: State;
  dispatch: React.Dispatch<Action>;
}) {
  const [loading, setLoading] = useState(!state.startResponse);

  useEffect(() => {
    if (state.startResponse) {
      return;
    }
    invoke<StartResponse>("commission_start")
      .then((response) => {
        dispatch({ type: "start_response", response });
        setLoading(false);
      })
      .catch((cause) => {
        const structured = asCommandError(cause);
        if (structured?.code === "tokenInvalid") {
          dispatch({
            type: "repair",
            initial: { stage: "needsTokenRefresh", agentName: null }
          });
        } else if (structured?.code === "alreadyCommissioned") {
          dispatch({
            type: "repair",
            initial: {
              stage: "needsRebind",
              agentId: structured.agentId ?? state.startResponse?.agentId ?? "unknown"
            }
          });
        } else {
          dispatch({ type: "error", message: formatError(cause) });
        }
        setLoading(false);
      });
  }, [dispatch, state.startResponse]);

  if (loading) {
    return (
      <section className="commissioning-panel">
        <h2>Identity key</h2>
        <p className="commissioning-loading">
          <Loader2 size={16} className="commissioning-spin" /> Generating
          mentor and entity keys in the SAO Vault...
        </p>
      </section>
    );
  }

  const start = state.startResponse;
  if (!start) {
    return (
      <section className="commissioning-panel">
        <h2>Identity key</h2>
        <p>SAO did not respond. Use the Repair stage if you have a fresh token.</p>
      </section>
    );
  }

  return (
    <section className="commissioning-panel">
      <h2>Identity key</h2>
      <p>
        SAO Vault has minted the mentor and entity keypairs for this agent.
        Private halves stay in the vault; OrionII never holds them. The
        fingerprints below are what SAO admin and audit logs reference.
      </p>
      <div className="commissioning-keys">
        <KeyRow icon="mentor" label="Mentor public key" fpr={start.mentorPublicKeyFpr} />
        <KeyRow icon="entity" label="Entity public key" fpr={start.entityPublicKeyFpr} />
      </div>
      <div className="commissioning-actions">
        <button
          type="button"
          className="commissioning-primary"
          onClick={() => dispatch({ type: "goto", stage: "chooseRole" })}
        >
          <span>Continue</span>
          <ArrowRight size={16} />
        </button>
      </div>
    </section>
  );
}

function KeyRow({
  icon,
  label,
  fpr
}: {
  icon: "mentor" | "entity";
  label: string;
  fpr: string;
}) {
  return (
    <div className="commissioning-key-row">
      <div className="commissioning-key-icon" aria-hidden="true">
        {icon === "mentor" ? <Fingerprint size={18} /> : <KeyRound size={18} />}
      </div>
      <div>
        <span>{label}</span>
        <code>{fpr}</code>
      </div>
    </div>
  );
}

function ChooseRole({ dispatch }: { dispatch: React.Dispatch<Action> }) {
  const [roles, setRoles] = useState<RoleSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<RoleSummary[]>("list_commissioning_roles")
      .then(setRoles)
      .catch((cause) => setError(formatError(cause)));
  }, []);

  if (error) {
    return (
      <section className="commissioning-panel">
        <h2>Choose role</h2>
        <p className="commissioning-error-inline">Failed to load roles: {error}</p>
      </section>
    );
  }

  if (!roles) {
    return (
      <section className="commissioning-panel">
        <h2>Choose role</h2>
        <p className="commissioning-loading">
          <Loader2 size={16} className="commissioning-spin" /> Loading roles...
        </p>
      </section>
    );
  }

  return (
    <section className="commissioning-panel">
      <h2>Choose role</h2>
      <p>
        Pick a fast-path template if your need lines up with a familiar
        function, or describe it freely on the Q&amp;A path. Either way you
        will review and edit the charter before SAO registers it.
      </p>
      <div className="commissioning-role-grid">
        {roles.map((role) => (
          <article
            key={role.key}
            className="commissioning-role-card"
            onClick={() =>
              dispatch({ type: "select_role", role, path: "fast" })
            }
          >
            <header>
              <h3>{role.displayName}</h3>
              <span>~{role.timeEstimateMinutes} min</span>
            </header>
            <p>{role.description}</p>
            <button type="button" className="commissioning-secondary">
              <span>Use fast template</span>
              <ArrowRight size={14} />
            </button>
          </article>
        ))}
      </div>
      <div className="commissioning-qna-card">
        <div className="commissioning-qna-header">
          <MessageSquare size={18} />
          <h3>Describe what you need (Q&amp;A path)</h3>
          <span>~5 min</span>
        </div>
        <p>
          If none of the templates fit, write a short description and let
          SAO draft a starting charter for you. v0 is single-shot; we will
          add a multi-turn dialog in v1.1.
        </p>
        <button
          type="button"
          className="commissioning-secondary"
          onClick={() =>
            // We need a role for slot reuse; the Q&A path uses the first
            // role purely as a placeholder host (its slots are unused).
            roles.length > 0
              ? dispatch({ type: "select_role", role: roles[0], path: "qna" })
              : null
          }
        >
          <Wand2 size={14} />
          <span>Start Q&amp;A path</span>
        </button>
      </div>
    </section>
  );
}

function DefineCharterFast({
  state,
  dispatch
}: {
  state: State;
  dispatch: React.Dispatch<Action>;
}) {
  const role = state.selectedRole;
  const [rendering, setRendering] = useState(false);

  if (!role) {
    return null;
  }

  const allFilled = useMemo(
    () => role.slots.every((slot) => state.slotValues[slot.key]?.trim()),
    [role.slots, state.slotValues]
  );

  async function generate() {
    if (!role) return;
    setRendering(true);
    try {
      const text = await invoke<string>("render_charter_from_role", {
        roleKey: role.key,
        slotValues: state.slotValues
      });
      dispatch({ type: "set_charter", text });
      dispatch({ type: "goto", stage: "reviewCharter" });
    } catch (cause) {
      dispatch({ type: "error", message: formatError(cause) });
    } finally {
      setRendering(false);
    }
  }

  return (
    <section className="commissioning-panel">
      <h2>{role.displayName}</h2>
      <p>{role.description}</p>
      <div className="commissioning-form">
        {role.slots.map((slot) => (
          <label key={slot.key}>
            <span>{slot.label}</span>
            {slot.kind === "multiline" ? (
              <textarea
                value={state.slotValues[slot.key] ?? ""}
                onChange={(e) =>
                  dispatch({
                    type: "set_slot",
                    key: slot.key,
                    value: e.target.value
                  })
                }
                rows={4}
              />
            ) : (
              <input
                type="text"
                value={state.slotValues[slot.key] ?? ""}
                onChange={(e) =>
                  dispatch({
                    type: "set_slot",
                    key: slot.key,
                    value: e.target.value
                  })
                }
              />
            )}
          </label>
        ))}
      </div>
      <div className="commissioning-actions">
        <button
          type="button"
          className="commissioning-secondary"
          onClick={() => dispatch({ type: "goto", stage: "chooseRole" })}
        >
          Back
        </button>
        <button
          type="button"
          className="commissioning-primary"
          onClick={generate}
          disabled={!allFilled || rendering}
        >
          {rendering ? <Loader2 size={16} className="commissioning-spin" /> : <ArrowRight size={16} />}
          <span>Generate charter</span>
        </button>
      </div>
    </section>
  );
}

function DefineCharterQna({
  state,
  dispatch
}: {
  state: State;
  dispatch: React.Dispatch<Action>;
}) {
  const [generating, setGenerating] = useState(false);

  async function generate() {
    if (!state.qnaDescription.trim()) {
      return;
    }
    setGenerating(true);
    try {
      const text = await invoke<string>("commission_qna", {
        description: state.qnaDescription
      });
      dispatch({ type: "set_charter", text });
      dispatch({ type: "goto", stage: "reviewCharter" });
    } catch (cause) {
      dispatch({ type: "error", message: formatError(cause) });
    } finally {
      setGenerating(false);
    }
  }

  return (
    <section className="commissioning-panel">
      <h2>Describe what you need</h2>
      <p>
        Tell SAO, in your own words, what work this agent should do, what
        systems it should reach into, who it serves, and what it must
        never do without your approval. SAO will draft a Markdown charter
        you can edit on the next screen.
      </p>
      <p className="commissioning-note">
        v0: single-turn. Be specific — domain, tasks, tools, output
        format, audience, and explicit boundaries. The more concrete you
        are, the better the draft.
      </p>
      <textarea
        className="commissioning-qna-textarea"
        rows={8}
        value={state.qnaDescription}
        onChange={(e) => dispatch({ type: "set_qna", description: e.target.value })}
        placeholder="e.g. I need an agent that triages my inbox each morning..."
      />
      <div className="commissioning-actions">
        <button
          type="button"
          className="commissioning-secondary"
          onClick={() => dispatch({ type: "goto", stage: "chooseRole" })}
        >
          Back
        </button>
        <button
          type="button"
          className="commissioning-primary"
          onClick={generate}
          disabled={!state.qnaDescription.trim() || generating}
        >
          {generating ? <Loader2 size={16} className="commissioning-spin" /> : <Wand2 size={16} />}
          <span>Draft charter</span>
        </button>
      </div>
    </section>
  );
}

function ReviewCharter({
  state,
  dispatch
}: {
  state: State;
  dispatch: React.Dispatch<Action>;
}) {
  return (
    <section className="commissioning-panel">
      <h2>Review charter</h2>
      <p>
        This charter will be signed by your mentor key and the entity key
        in SAO Vault, then registered in SAO's commissioning archive. Edit
        anything here before continuing — this is your last chance to
        change wording without going through a charter amendment.
      </p>
      <textarea
        className="commissioning-charter-textarea"
        rows={20}
        value={state.charterDraft}
        onChange={(e) => dispatch({ type: "set_charter", text: e.target.value })}
        spellCheck={false}
      />
      <div className="commissioning-actions">
        <button
          type="button"
          className="commissioning-secondary"
          onClick={() =>
            dispatch({
              type: "goto",
              stage: state.selectedPath === "fast" ? "defineCharterFast" : "defineCharterQna"
            })
          }
        >
          Back
        </button>
        <button
          type="button"
          className="commissioning-primary"
          disabled={!state.charterDraft.trim()}
          onClick={() => dispatch({ type: "goto", stage: "register" })}
        >
          <span>Register with SAO</span>
          <ArrowRight size={16} />
        </button>
      </div>
    </section>
  );
}

function RegisterWithSao({
  state,
  dispatch
}: {
  state: State;
  dispatch: React.Dispatch<Action>;
}) {
  const [step, setStep] = useState<"signing" | "registering" | "certificate" | "done">("signing");

  useEffect(() => {
    let cancelled = false;
    async function run() {
      try {
        if (!state.startResponse) {
          throw new Error("Internal: missing start response");
        }
        if (cancelled) return;
        setStep("signing");
        const tickStart = Date.now();
        // Brief minimum dwell so the operator perceives the cryptographic
        // act, not a flash of green checkmarks.
        await new Promise((resolve) => setTimeout(resolve, 400));

        if (cancelled) return;
        setStep("registering");
        const result = await invoke<FinalizeResult>("commission_finalize", {
          commissionId: state.startResponse.commissionId,
          roleKey: state.selectedRole?.key ?? "qna_drafted",
          charterText: state.charterDraft
        });

        if (cancelled) return;
        setStep("certificate");
        const elapsed = Date.now() - tickStart;
        if (elapsed < 1200) {
          await new Promise((resolve) => setTimeout(resolve, 1200 - elapsed));
        }

        if (cancelled) return;
        setStep("done");
        dispatch({ type: "finalized", result });
      } catch (cause) {
        if (!cancelled) {
          dispatch({ type: "error", message: formatError(cause) });
          dispatch({ type: "goto", stage: "reviewCharter" });
        }
      }
    }
    void run();
    return () => {
      cancelled = true;
    };
  }, [dispatch, state.charterDraft, state.selectedRole?.key, state.startResponse]);

  const tick = (active: boolean, done: boolean, label: string) => (
    <li className={`commissioning-step ${done ? "done" : active ? "active" : ""}`}>
      {done ? (
        <CheckCircle2 size={16} />
      ) : active ? (
        <Loader2 size={14} className="commissioning-spin" />
      ) : (
        <span className="commissioning-step-dot" />
      )}
      <span>{label}</span>
    </li>
  );

  return (
    <section className="commissioning-panel">
      <h2>Registering with SAO</h2>
      <ol className="commissioning-steps">
        {tick(step === "signing", step !== "signing", "Signing charter with mentor and entity keys")}
        {tick(step === "registering", step === "certificate" || step === "done", "Registering charter in SAO")}
        {tick(step === "certificate", step === "done", "Receiving birth certificate")}
      </ol>
    </section>
  );
}

function Ready({
  state,
  onCommissioned
}: {
  state: State;
  onCommissioned: () => void;
}) {
  return (
    <section className="commissioning-panel">
      <h2>Agent commissioned</h2>
      <p>
        Charter v1 signed by you and SAO. The entity is now operating
        under a content-addressed soul reference; every event on the bus
        is auditable against the registered charter.
      </p>
      <div className="commissioning-summary">
        <div>
          <span>Soul ref</span>
          <code>{state.finalizeResult?.soulRef ?? "unknown"}</code>
        </div>
        <div>
          <span>Charter hash</span>
          <code>{state.finalizeResult?.charterHash ?? "unknown"}</code>
        </div>
      </div>
      <div className="commissioning-actions">
        <button
          type="button"
          className="commissioning-primary"
          onClick={onCommissioned}
        >
          <span>Open cockpit</span>
          <ArrowRight size={16} />
        </button>
      </div>
    </section>
  );
}

function Repair({
  initial,
  dispatch,
  onCommissioned
}: {
  initial: CommissioningStateView;
  dispatch: React.Dispatch<Action>;
  onCommissioned: () => void;
}) {
  const [credentialText, setCredentialText] = useState("");
  const [busy, setBusy] = useState(false);
  const isBundleConfig = looksLikeBundleConfig(credentialText);

  async function refreshCredentials() {
    const text = credentialText.trim();
    if (!text) return;
    setBusy(true);
    try {
      if (looksLikeBundleConfig(text)) {
        await invoke("apply_bundle_config", { json: text });
      } else {
        await invoke<FinalizeResult>("commission_repair", {
          request: { kind: "rotate_token", newToken: text }
        });
      }
      onCommissioned();
    } catch (cause) {
      dispatch({ type: "error", message: formatError(cause) });
    } finally {
      setBusy(false);
    }
  }

  async function rebind() {
    setBusy(true);
    try {
      await invoke<FinalizeResult>("commission_repair", {
        request: { kind: "rebind" }
      });
      onCommissioned();
    } catch (cause) {
      dispatch({ type: "error", message: formatError(cause) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="commissioning-panel">
      <h2>Repair commissioning</h2>
      {initial.stage === "needsTokenRefresh" ? (
        <>
          <p>
            SAO rejected the bundled token (HTTP 401/403). The mentor and
            entity keys are still bound to{" "}
            <strong>{initial.agentName ?? "this agent"}</strong>; only the
            bearer needs to refresh.
          </p>
          <p>Paste either the refreshed token or the full bundle config from SAO:</p>
          <textarea
            rows={6}
            value={credentialText}
            onChange={(e) => setCredentialText(e.target.value)}
            placeholder='eyJhbGciOi... or {"sao_base_url":"http://localhost:3100","agent_token":"..."}'
          />
          <div className="commissioning-actions">
            <button
              type="button"
              className="commissioning-primary"
              disabled={!credentialText.trim() || busy}
              onClick={refreshCredentials}
            >
              {busy ? <Loader2 size={16} className="commissioning-spin" /> : <RefreshCw size={16} />}
              <span>{isBundleConfig ? "Apply bundle config" : "Refresh credentials"}</span>
            </button>
          </div>
        </>
      ) : initial.stage === "needsRebind" ? (
        <>
          <p>
            A bundle for agent <code>{initial.agentId}</code> is on disk
            but the local charter and certificate are missing. Re-bind to
            recover them from SAO's commissioning archive.
          </p>
          <div className="commissioning-actions">
            <button
              type="button"
              className="commissioning-primary"
              disabled={busy}
              onClick={rebind}
            >
              {busy ? <Loader2 size={16} className="commissioning-spin" /> : <Fingerprint size={16} />}
              <span>Re-bind to existing agent</span>
            </button>
          </div>
        </>
      ) : (
        <p>No repair needed.</p>
      )}
    </section>
  );
}

type CommissioningCommandError = {
  code?: string;
  message?: string;
  agentId?: string;
};

function asCommandError(cause: unknown): CommissioningCommandError | null {
  if (!cause || typeof cause !== "object") {
    return null;
  }

  const shaped = cause as CommissioningCommandError;
  return typeof shaped.code === "string" ? shaped : null;
}

function looksLikeBundleConfig(value: string): boolean {
  return value.trim().startsWith("{");
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

export default Commissioning;
