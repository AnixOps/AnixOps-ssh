import { CircleAlert, ChevronDown, ChevronRight, Plus, ShieldAlert, ShieldCheck, UserMinus, UserPlus, Users } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { SafetyNumber } from "@/components/app/SyncSettings";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import { Tooltip } from "@/components/ui/tooltip";
import { t } from "@/lib/i18n";
import {
  addMember,
  createSharedVault,
  listVaults,
  lookupMember,
  removeMember,
  setMemberRole,
  vaultMembers,
  type VaultInfo,
  type VaultMember,
  type VaultMembersLookupResult,
} from "@/lib/syncApi";
import { syncErrorText, useSyncStatus } from "@/lib/syncText";
import {
  canManageMembers,
  fingerprintGroups,
  isLastOwner,
  needsAttention,
  ROLES,
  sortMembers,
  type Role,
} from "@/lib/syncUi";
import { cn } from "@/lib/utils";

const ROLE_LABEL: Record<Role, string> = { owner: t.roleOwner, editor: t.roleEditor, viewer: t.roleViewer };
const TRUST_LABEL: Record<VaultMember["trust"], string> = {
  self: t.trustSelf,
  confirmed: t.trustConfirmed,
  unconfirmed: t.trustUnconfirmed,
  changed: t.trustChanged,
};

/** Team vaults: the vaults you share, who is in them, and the safety-number
 *  check before someone is added. */
export function TeamVaults() {
  const { status } = useSyncStatus();
  const signedIn = status?.signed_in ?? false;
  const [vaults, setVaults] = useState<VaultInfo[]>([]);
  const [open, setOpen] = useState<string | null>(null);
  const [creating, setCreating] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(() => {
    if (!signedIn) {
      setVaults([]);
      return;
    }
    listVaults()
      .then((list) => setVaults(list.filter((vault) => vault.kind === "team")))
      .catch((reason) => setError(syncErrorText(reason)));
  }, [signedIn]);
  useEffect(reload, [reload]);

  const create = async () => {
    if (!creating?.trim()) return;
    try {
      const vault = await createSharedVault(creating.trim());
      setCreating(null);
      setError(null);
      setOpen(vault.id);
      reload();
    } catch (reason) {
      setError(syncErrorText(reason));
    }
  };

  return (
    <aside className="material-sidebar flex w-72 shrink-0 flex-col border-r">
      <div className="flex items-center px-4 pt-4 pb-2">
        <h2 className="flex-1 text-title-3">{t.navTeam}</h2>
        {signedIn && (
          <Tooltip label={t.teamNew}>
            <Button variant="secondary" size="icon" aria-label={t.teamNew} onClick={() => setCreating("")}>
              <Plus />
            </Button>
          </Tooltip>
        )}
      </div>
      <div className="min-h-0 flex-1 space-y-3 overflow-y-auto px-3 pb-4">
        {error && (
          <p role="alert" className="flex items-start gap-2 rounded-sm bg-danger-soft px-3 py-2 text-caption font-normal text-danger">
            <CircleAlert className="mt-px size-3.5 shrink-0" strokeWidth={1.5} />
            <span className="selectable min-w-0 break-words">{error}</span>
          </p>
        )}
        {!signedIn ? (
          <div className="px-3 pt-10 text-center">
            <Users className="mx-auto mb-3 size-12 text-label-3" strokeWidth={1.5} />
            <p className="text-callout font-semibold text-label-1">{t.teamSignInFirst}</p>
          </div>
        ) : (
          <>
            <p className="px-1 text-caption font-normal text-label-2">{t.teamIntro}</p>
            {creating !== null && (
              <form
                className="flex gap-1.5"
                onSubmit={(event) => {
                  event.preventDefault();
                  void create();
                }}
              >
                <Input
                  autoFocus
                  value={creating}
                  onChange={(event) => setCreating(event.target.value)}
                  placeholder={t.teamNamePlaceholder}
                  aria-label={t.teamNamePlaceholder}
                  maxLength={64}
                  className="min-w-0 flex-1"
                />
                <Button type="submit" variant="primary" disabled={!creating.trim()}>
                  {t.teamCreate}
                </Button>
              </form>
            )}
            {vaults.length === 0 && creating === null && (
              <p className="px-3 pt-6 text-center text-caption font-normal text-label-2">{t.teamNone}</p>
            )}
            {vaults.map((vault) => (
              <VaultCard
                key={vault.id}
                vault={vault}
                expanded={open === vault.id}
                onToggle={() => setOpen(open === vault.id ? null : vault.id)}
                onError={setError}
              />
            ))}
            <p className="px-1 text-caption font-normal text-label-3">{t.teamRotates}</p>
          </>
        )}
      </div>
    </aside>
  );
}

function VaultCard({
  vault,
  expanded,
  onToggle,
  onError,
}: {
  vault: VaultInfo;
  expanded: boolean;
  onToggle: () => void;
  onError: (text: string | null) => void;
}) {
  const Chevron = expanded ? ChevronDown : ChevronRight;
  return (
    <section className="overflow-hidden rounded-md bg-bg-elevated shadow-1 ring-1 ring-separator">
      <button
        type="button"
        aria-expanded={expanded}
        onClick={onToggle}
        className="flex min-h-11 w-full items-center gap-2 px-3 py-1.5 text-left hover:bg-fill-1"
      >
        <Chevron className="size-4 shrink-0 text-label-2" strokeWidth={1.5} />
        <span className="min-w-0 flex-1 truncate text-callout text-label-1">{vault.name}</span>
        {vault.role && (
          <span className="shrink-0 text-caption font-normal text-label-2">
            {ROLE_LABEL[vault.role as Role] ?? vault.role}
          </span>
        )}
      </button>
      {expanded && <Members vault={vault} onError={onError} />}
    </section>
  );
}

function Members({ vault, onError }: { vault: VaultInfo; onError: (text: string | null) => void }) {
  const [members, setMembers] = useState<VaultMember[]>([]);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const manage = canManageMembers(vault.role);

  const reload = useCallback(() => {
    vaultMembers(vault.id)
      .then((list) => setMembers(sortMembers(list)))
      .catch((reason) => onError(syncErrorText(reason)));
  }, [vault.id, onError]);
  useEffect(reload, [reload]);

  const act = async (action: () => Promise<unknown>) => {
    try {
      await action();
      onError(null);
    } catch (reason) {
      onError(syncErrorText(reason));
    }
    reload();
  };

  return (
    <div className="divide-y divide-separator border-t border-separator">
      <p className="px-3 pt-2 pb-1 text-caption text-label-2">{t.teamMembers}</p>
      <ul>
        {members.map((member) => {
          const locked = member.trust === "self" || isLastOwner(members, member);
          return (
            <li key={member.account_id} className="px-3 py-2">
              <div className="flex items-center gap-2">
                <span className="min-w-0 flex-1 truncate text-callout text-label-1">{member.username}</span>
                {manage && !locked ? (
                  <Select
                    aria-label={`${t.teamRole}: ${member.username}`}
                    value={member.role}
                    onChange={(event) =>
                      void act(() => setMemberRole(vault.id, member.account_id, event.target.value as Role))
                    }
                    className="h-7 w-auto"
                  >
                    {ROLES.map((role) => (
                      <option key={role} value={role}>
                        {ROLE_LABEL[role]}
                      </option>
                    ))}
                  </Select>
                ) : (
                  <span className="shrink-0 text-caption font-normal text-label-2">{ROLE_LABEL[member.role]}</span>
                )}
                {manage &&
                  !locked &&
                  (confirming === member.account_id ? (
                    <Button
                      variant="destructive"
                      size="sm"
                      onClick={() => {
                        setConfirming(null);
                        void act(() => removeMember(vault.id, member.account_id));
                      }}
                    >
                      {t.teamRemoveConfirm}
                    </Button>
                  ) : (
                    <Tooltip label={`${t.teamRemove}: ${member.username}`}>
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        aria-label={`${t.teamRemove}: ${member.username}`}
                        onClick={() => setConfirming(member.account_id)}
                      >
                        <UserMinus />
                      </Button>
                    </Tooltip>
                  ))}
              </div>
              <p
                className={cn(
                  "mt-0.5 flex items-center gap-1 text-caption font-normal",
                  needsAttention(member.trust) ? "text-warning" : "text-label-3",
                )}
              >
                {needsAttention(member.trust) ? (
                  <ShieldAlert className="size-3.5 shrink-0" strokeWidth={1.5} />
                ) : (
                  <ShieldCheck className="size-3.5 shrink-0" strokeWidth={1.5} />
                )}
                {TRUST_LABEL[member.trust]}
              </p>
              {member.fingerprint && <SafetyNumber groups={fingerprintGroups(member.fingerprint)} />}
            </li>
          );
        })}
      </ul>
      {manage ? (
        adding ? (
          <AddMember
            vaultId={vault.id}
            onDone={() => {
              setAdding(false);
              reload();
            }}
            onCancel={() => setAdding(false)}
          />
        ) : (
          <div className="px-3 py-2">
            <Button variant="secondary" size="sm" onClick={() => setAdding(true)}>
              <UserPlus />
              {t.teamAddMember}
            </Button>
          </div>
        )
      ) : (
        <p className="px-3 py-2 text-caption font-normal text-label-3">{t.teamOwnerOnly}</p>
      )}
    </div>
  );
}

/** Look the account up, show the safety number the server's keys give, and
 *  add them only after the owner says it matches what the other person sees. */
function AddMember({ vaultId, onDone, onCancel }: { vaultId: string; onDone: () => void; onCancel: () => void }) {
  const [username, setUsername] = useState("");
  const [found, setFound] = useState<VaultMembersLookupResult | null>(null);
  const [role, setRole] = useState<Role>("editor");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const lookup = async () => {
    setBusy(true);
    setError(null);
    try {
      setFound(await lookupMember(username.trim()));
    } catch (reason) {
      setFound(null);
      setError(syncErrorText(reason));
    } finally {
      setBusy(false);
    }
  };

  const add = async () => {
    if (!found) return;
    setBusy(true);
    setError(null);
    try {
      await addMember(vaultId, found.username, role, found.fingerprint);
      onDone();
    } catch (reason) {
      setError(syncErrorText(reason));
      setBusy(false);
    }
  };

  return (
    <div className="space-y-2 px-3 py-3">
      <form
        className="flex gap-1.5"
        onSubmit={(event) => {
          event.preventDefault();
          if (username.trim()) void lookup();
        }}
      >
        <Input
          autoFocus
          value={username}
          onChange={(event) => {
            setUsername(event.target.value);
            setFound(null);
          }}
          placeholder={t.teamUsername}
          aria-label={t.teamUsername}
          autoComplete="off"
          spellCheck={false}
          className="h-8 min-w-0 flex-1 text-caption font-normal"
        />
        <Button type="submit" variant="secondary" size="sm" className="h-8" disabled={!username.trim() || busy}>
          {t.teamLookup}
        </Button>
      </form>
      {found && (
        <>
          <p className="text-caption text-label-1">
            {t.teamSafetyNumber}: {found.username}
          </p>
          <SafetyNumber groups={fingerprintGroups(found.fingerprint)} />
          <p className="text-caption font-normal text-warning">{t.teamCompare}</p>
          <Select
            aria-label={t.teamRole}
            value={role}
            onChange={(event) => setRole(event.target.value as Role)}
            className="h-8"
          >
            {ROLES.map((r) => (
              <option key={r} value={r}>
                {ROLE_LABEL[r]}
              </option>
            ))}
          </Select>
        </>
      )}
      {error && (
        <p role="alert" className="flex items-start gap-2 text-caption font-normal break-words text-danger">
          <CircleAlert className="mt-px size-3.5 shrink-0" strokeWidth={1.5} />
          <span className="selectable min-w-0">{error}</span>
        </p>
      )}
      <div className="flex justify-end gap-1.5">
        <Button variant="ghost" size="sm" onClick={onCancel}>
          {t.cancel}
        </Button>
        {found && (
          <Button variant="primary" size="sm" disabled={busy} onClick={() => void add()}>
            {t.teamMatches}
          </Button>
        )}
      </div>
    </div>
  );
}
