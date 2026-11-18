import { ArrowRightLeft, Pencil, Plus, Trash2 } from "lucide-react";
import { AnimatePresence } from "motion/react";
import { useCallback, useEffect, useState } from "react";
import { MobileForwardForm } from "@/components/mobile/MobileForwardForm";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm";
import { Notice } from "@/components/ui/form";
import { Menu } from "@/components/ui/menu";
import {
  deleteTunnel,
  loadTunnels,
  onTunnelStatus,
  startTunnel,
  stopTunnel,
  type Host,
  type Tunnel,
  type TunnelStatus,
} from "@/lib/backend";
import { t } from "@/lib/i18n";
import { describe } from "@/lib/mobileForwards";
import { cn } from "@/lib/utils";

function statusText(status: TunnelStatus): string {
  switch (status.state) {
    case "running":
      return t.tunnelRunning;
    case "starting":
      return t.tunnelStarting;
    case "failed":
      return status.message;
    default:
      return t.tunnelStopped;
  }
}

/** The port forwards of the phone (`ssh -L`): a port on the phone that reaches
 *  a service behind a saved host. A forward runs while the app does; the
 *  switch starts and stops it. */
export function MobileForwards({ hosts }: { hosts: Host[] }) {
  const [tunnels, setTunnels] = useState<Tunnel[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<Tunnel | "new" | null>(null);
  const confirm = useConfirm();

  const reload = useCallback(() => {
    loadTunnels()
      .then((list) => {
        setTunnels(list);
        setError(null);
      })
      .catch((reason) => setError(String(reason)));
  }, []);
  useEffect(reload, [reload]);
  useEffect(
    () =>
      onTunnelStatus((id, status) =>
        setTunnels((current) => current.map((tunnel) => (tunnel.id === id ? { ...tunnel, status } : tunnel))),
      ),
    [],
  );

  const hostName = (id: string) => hosts.find((host) => host.id === id)?.name ?? "?";
  const usable = hosts.some((host) => host.fields.serial == null);

  const toggle = (tunnel: Tunnel) => {
    const running = tunnel.status.state === "running" || tunnel.status.state === "starting";
    (running ? stopTunnel(tunnel.id) : startTunnel(tunnel.id))
      .then(() => setError(null))
      .catch((reason) => setError(String(reason)));
  };

  const remove = async (tunnel: Tunnel) => {
    const ok = await confirm({
      title: t.tunnelDeleteTitle.replace("{name}", tunnel.name),
      body: t.tunnelDeleteBody,
      verb: t.delete,
    });
    if (ok) deleteTunnel(tunnel.id).then(reload).catch((reason) => setError(String(reason)));
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="material-bar pt-safe shrink-0 border-b">
        <div className="px-safe flex h-14 items-center justify-between gap-2">
          <h1 className="truncate text-title-3">{t.mobileTabForwards}</h1>
          <Button
            variant="quiet"
            size="icon-lg"
            aria-label={t.newTunnel}
            disabled={!usable}
            onClick={() => setEditing("new")}
          >
            <Plus />
          </Button>
        </div>
      </header>

      {error && (
        <Notice className="mx-4 mt-3">
          {error}
        </Notice>
      )}

      {tunnels.length === 0 ? (
        <div className="px-safe flex flex-1 flex-col items-center justify-center gap-2 text-center">
          <ArrowRightLeft className="size-8 text-label-3" strokeWidth={1.5} />
          <p className="text-body font-medium">{t.noTunnels}</p>
          <p className="max-w-xs text-callout text-label-2">{usable ? t.mobileNoForwardsHint : t.mobileForwardNeedsHost}</p>
          {usable && (
            <Button variant="primary" size="lg" className="mt-4" onClick={() => setEditing("new")}>
              <Plus />
              {t.newTunnel}
            </Button>
          )}
        </div>
      ) : (
        <ul className="min-h-0 flex-1 overflow-y-auto pb-2">
          {tunnels.map((tunnel) => {
            const on = tunnel.status.state === "running" || tunnel.status.state === "starting";
            return (
              <li key={tunnel.id} className="flex items-center border-b pr-2">
                <div className="px-safe min-w-0 flex-1 py-2">
                  <p className="truncate text-body text-label-1">{tunnel.name}</p>
                  <p className="truncate text-callout text-label-2">
                    {describe(tunnel)} · {hostName(tunnel.hostId)}
                  </p>
                  <p
                    role="status"
                    className={cn("truncate text-caption", tunnel.status.state === "failed" ? "text-danger" : "text-label-3")}
                  >
                    {statusText(tunnel.status)}
                  </p>
                </div>
                <button
                  type="button"
                  role="switch"
                  aria-checked={on}
                  aria-label={`${on ? t.stopTunnel : t.startTunnel}: ${tunnel.name}`}
                  onClick={() => toggle(tunnel)}
                  className="flex size-11 shrink-0 items-center justify-center"
                >
                  <span
                    aria-hidden
                    className={cn(
                      "relative block h-6 w-10 rounded-pill transition-colors duration-toggle ease-standard",
                      on ? "bg-accent-fill" : "bg-fill-3",
                    )}
                  >
                    <span
                      className={cn(
                        "absolute top-0.5 left-0.5 size-5 rounded-full bg-(--on-accent) shadow-1 transition-transform duration-toggle ease-emphasized",
                        on && "translate-x-4",
                      )}
                    />
                  </span>
                </button>
                <Menu
                  touch
                  label={`${t.more}: ${tunnel.name}`}
                  items={[
                    {
                      key: "edit",
                      label: t.edit,
                      icon: <Pencil />,
                      // Remote forwards and SOCKS proxies are made on the desktop.
                      disabled: tunnel.kind !== "local",
                      onSelect: () => setEditing(tunnel),
                    },
                    "separator",
                    { key: "delete", label: t.delete, icon: <Trash2 />, danger: true, onSelect: () => void remove(tunnel) },
                  ]}
                />
              </li>
            );
          })}
        </ul>
      )}

      <AnimatePresence>
        {editing && (
          <MobileForwardForm
            tunnel={editing === "new" ? undefined : editing}
            hosts={hosts}
            onCancel={() => setEditing(null)}
            onDone={() => {
              setEditing(null);
              reload();
            }}
          />
        )}
      </AnimatePresence>
    </div>
  );
}
