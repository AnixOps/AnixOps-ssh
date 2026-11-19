import type {
  Host,
  HostFields,
  HostInput,
  HostsImportApplyParams,
  HostsImportApplyResult,
  HostsImportPreviewResult,
  HostsSnapshot,
  ImportFormat,
  ImportHost,
  KnownHost,
  KnownHostsImport,
  ServiceForm,
  ServiceId,
  ServiceRemembered,
  ServiceRequest,
} from "./backend";

/** Mirrors `display_target` and `display_command` in src-tauri/src/hosts.rs. */
function describe(fields: HostFields): { target: string; command: string } {
  const serial = fields.serial;
  if (serial) {
    const parity = { none: "N", odd: "O", even: "E" }[serial.parity];
    return {
      target: `${serial.port} · ${serial.baud} ${serial.dataBits}${parity}${serial.stopBits}`,
      command: `screen ${serial.port} ${serial.baud}`,
    };
  }
  if (fields.alias) return { target: fields.alias, command: `ssh ${fields.alias}` };
  const user = fields.username ? `${fields.username}@` : "";
  const port = fields.port === 22 ? "" : `:${fields.port}`;
  const flag = fields.port === 22 ? "" : `-p ${fields.port} `;
  return { target: `${user}${fields.hostname}${port}`, command: `ssh ${flag}${user}${fields.hostname}` };
}

function endpoint(target: string): HostFields {
  const [userPart, rest] = target.includes("@") ? target.split("@") : [null, target];
  const [hostname, port] = rest.split(":");
  return { alias: null, hostname, username: userPart, port: port ? Number(port) : 22, proxyJump: null, notes: "", shellIntegration: false };
}

function makeHost(
  id: string,
  name: string,
  fields: HostFields,
  groupId: string | null,
  extra: { favorite?: boolean; color?: string; source?: Host["source"] } = {},
): Host {
  return {
    id,
    name,
    ...describe(fields),
    groupId,
    tags: [],
    favorite: extra.favorite ?? false,
    color: extra.color ?? null,
    source: extra.source ?? "saved",
    fields,
  };
}

/** Sample data for browser previews and CI screenshots, editable in memory.
 *  `?hosts=empty` shows the first-run state. */
const store: HostsSnapshot =
  new URLSearchParams(window.location.search).get("hosts") === "empty"
    ? { hosts: [], groups: [], warnings: [] }
    : {
        groups: [
          { id: "prod", name: "生产", color: null },
          { id: "staging", name: "测试", color: null },
          { id: "home", name: "个人", color: null },
        ],
        hosts: [
          makeHost("1", "prod-web-01", endpoint("deploy@10.0.1.11"), "prod", { favorite: true, color: "chart-1" }),
          makeHost("2", "prod-web-02", endpoint("deploy@10.0.1.12"), "prod"),
          makeHost("3", "prod-db", endpoint("postgres@10.0.2.20"), "prod", { favorite: true, color: "chart-5" }),
          makeHost("4", "staging-api", endpoint("ubuntu@staging.internal:2222"), "staging"),
          makeHost("5", "home-nas", endpoint("admin@192.168.1.10"), "home", { favorite: true }),
          makeHost("6", "raspberry-pi", endpoint("pi@192.168.1.42"), "home", { color: "chart-7" }),
          makeHost(
            "7",
            "switch-console",
            {
              alias: null,
              hostname: "",
              username: null,
              port: 22,
              proxyJump: null,
              notes: "",
              shellIntegration: false,
              serial: { port: "/dev/ttyUSB0", baud: 9600, dataBits: 8, parity: "none", stopBits: 1, flowControl: "none" },
            },
            "home",
          ),
          makeHost(
            "ssh-config:bastion",
            "bastion",
            { alias: "bastion", hostname: "", username: null, port: 22, proxyJump: null, notes: "", shellIntegration: false },
            null,
            { source: "ssh_config" },
          ),
        ],
        warnings: [],
      };

/** Ports the preview pretends to find. */
export function previewSerialPorts(): { path: string; description: string | null }[] {
  return [
    { path: "/dev/ttyACM0", description: "Arduino Uno (USB 2341:0043)" },
    { path: "/dev/ttyUSB0", description: "FTDI FT232R USB UART (USB 0403:6001)" },
  ];
}

export function previewSnapshot(): HostsSnapshot {
  return structuredClone(store);
}

let nextId = 100;

export function previewSaveHost(input: HostInput): string {
  const groupName = input.group?.trim();
  let groupId: string | null = null;
  if (groupName) {
    const existing = store.groups.find((group) => group.name.toLowerCase() === groupName.toLowerCase());
    groupId = existing?.id ?? `group-${nextId++}`;
    if (!existing) store.groups.push({ id: groupId, name: groupName, color: null });
  }
  const saved = store.hosts.find((host) => host.id === input.id && host.source === "saved");
  const id = saved?.id ?? String(nextId++);
  const host = makeHost(id, input.name.trim(), input.fields, groupId, {
    favorite: input.favorite,
    color: input.color ?? undefined,
  });
  host.tags = input.tags;
  // Saving a ~/.ssh/config host turns it into a saved host, as in the app.
  if (input.id?.startsWith("ssh-config:")) store.hosts = store.hosts.filter((entry) => entry.id !== input.id);
  if (saved) store.hosts[store.hosts.indexOf(saved)] = host;
  else store.hosts.push(host);
  return id;
}

export function previewDeleteHost(id: string): void {
  store.hosts = store.hosts.filter((host) => host.id !== id);
}

export function previewSetFavorite(id: string, favorite: boolean): void {
  const host = store.hosts.find((candidate) => candidate.id === id);
  if (host) host.favorite = favorite;
}

function sampleKnownHost(id: string, host: string, fingerprint: string, comment: string | null = null): KnownHost {
  return {
    id,
    host,
    port: 22,
    hashed: false,
    marker: null,
    keyType: "ssh-ed25519",
    fingerprint,
    comment,
    addedAt: "2026-01-01T00:00:00+00:00",
  };
}

/** Sample known hosts; `?hosts=empty` starts without any. */
let knownHosts: KnownHost[] =
  new URLSearchParams(window.location.search).get("hosts") === "empty"
    ? []
    : [
        sampleKnownHost("k1", "prod-web-01", "SHA256:3hL9r0xq2vGkX1uWZc5a8mT7yBnJdQe4FpRsUoVtKiA", "deploy@laptop"),
        sampleKnownHost("k2", "github.com", "SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU"),
      ];

export function previewKnownHosts(): KnownHost[] {
  return structuredClone(knownHosts);
}

export function previewImportKnownHosts(): KnownHostsImport {
  const sample = sampleKnownHost("k3", "home-nas", "SHA256:qT5nD8aVwYb2KcXz0LrHj7mEoUfG1sPiN4tZ9xAeCkI");
  if (knownHosts.some((entry) => entry.id === sample.id)) return { imported: 0, duplicates: 1, skipped: [] };
  knownHosts = [...knownHosts, sample];
  return { imported: 1, duplicates: 0, skipped: [] };
}

export function previewRemoveKnownHost(id: string): void {
  knownHosts = knownHosts.filter((entry) => entry.id !== id);
}

const sshConfigHosts: ImportHost[] = [
  { label: "prod-web-01", hostname: "10.0.1.11", port: 22, username: "deploy", exists: true },
  { label: "prod-web-02", hostname: "10.0.1.12", port: 22, username: "deploy", exists: true },
  { label: "build-box", hostname: "build.example.com", port: 2222, username: "ci", exists: false },
  { label: "bastion-eu", hostname: "bastion.eu.example.com", port: 22, username: "ops", exists: false },
  { label: "db-replica", hostname: "192.0.2.30", port: 22, username: "postgres", jump: ["bastion-eu"], exists: false },
  {
    label: "legacy-switch",
    hostname: "192.0.2.7",
    port: 22,
    username: "admin",
    openssh_only: ["ProxyCommand", "KexAlgorithms"],
    exists: false,
  },
  { label: "quoted alias", hostname: "quoted.example.com", port: 22, username: "quoted user", exists: false, problem: "username is not acceptable" },
];

const fileHosts: ImportHost[] = [
  { label: "web-1", hostname: "web1.example.com", port: 2222, username: "alice", group_path: ["Prod", "Web"], exists: false },
  { label: "web-2", hostname: "web2.example.com", port: 22, username: "alice", group_path: ["Prod", "Web"], exists: false },
  { label: "db", hostname: "192.0.2.20", port: 22, username: "dbadmin", group_path: ["Prod"], exists: false },
  { label: "prod-web-01", hostname: "10.0.1.11", port: 22, username: "deploy", group_path: ["Prod"], exists: true },
];

/** A canned preview of an import: `~/.ssh/config` or a picked file. */
export function previewImport(kind: "file" | "ssh_config", format: ImportFormat | null): HostsImportPreviewResult {
  const detected: ImportFormat = kind === "ssh_config" ? "openssh" : "csv";
  const hosts = structuredClone(kind === "ssh_config" ? sshConfigHosts : fileHosts);
  const groups = new Map<string, string[]>();
  for (const host of hosts)
    for (let end = 1; end <= (host.group_path?.length ?? 0); end++) {
      const path = host.group_path!.slice(0, end);
      groups.set(path.join("/"), path);
    }
  return {
    format: format ?? detected,
    detected,
    hosts,
    groups: [...groups.values()],
    warnings:
      kind === "ssh_config"
        ? ["Include ~/.ssh/config.d/*.conf: 2 files read", "legacy-switch: Match blocks are not evaluated"]
        : [],
    skipped: kind === "ssh_config" ? 1 : 0,
  };
}

/** Settings the service form remembers in the browser preview (not the token). */
const rememberedServices = new Map<ServiceId, ServiceRemembered>();

export function previewServiceRemembered(service: ServiceId): ServiceRemembered {
  return structuredClone(
    rememberedServices.get(service) ?? {
      form: {
        url: "",
        default_user: "root",
        identity_file: null,
        access_key_id: "",
        org_id: "",
        gateway: true,
        gateway_host: "",
        gateway_port: 2222,
        gateway_user: "",
      } satisfies ServiceForm,
      has_secret: false,
    },
  );
}

function checkServiceRequest(request: ServiceRequest): void {
  if (!request.form.url.trim()) throw new Error("Enter the address of the service.");
  if (!/^https:\/\/|^http:\/\/(localhost|127\.0\.0\.1)/.test(request.form.url.trim()))
    throw new Error("The address must start with https:// (http:// is accepted for localhost only).");
  const saved = rememberedServices.get(request.service)?.has_secret && !request.clear_secret;
  if (!request.secret?.trim() && !saved) throw new Error("Enter an admin API token.");
  if (request.service === "control" && request.secret && !request.secret.startsWith("anixadm_"))
    throw new Error("An admin API token starts with anixadm_. Create one in the panel under API tokens.");
  if (request.service === "jumpserver" && !request.form.access_key_id.trim())
    throw new Error("Enter the access key id (create an access key under Profile, API key in JumpServer).");
  if (request.service === "jumpserver" && request.form.gateway && !request.form.gateway_user.trim())
    throw new Error("Enter your JumpServer user name: it is the SSH user name at the gateway.");
}

function rememberService(request: ServiceRequest): void {
  rememberedServices.set(request.service, {
    form: structuredClone(request.form),
    has_secret: Boolean(request.secret?.trim()) || (rememberedServices.get(request.service)?.has_secret && !request.clear_secret) || false,
  });
}

export function previewServiceTest(request: ServiceRequest): number {
  checkServiceRequest(request);
  rememberService(request);
  return request.service === "jumpserver" ? 7 : 5;
}

/** A canned preview of an AnixOps Control import; the second node is stored already. */
export function previewServicePreview(request: ServiceRequest): HostsImportPreviewResult {
  checkServiceRequest(request);
  rememberService(request);
  if (request.service === "jumpserver") return previewJumpServer(request);
  const panel = new URL(request.form.url.trim()).hostname;
  const user = request.form.default_user.trim() || null;
  const node = (id: number, label: string, hostname: string, group: string, tags: string[], exists = false): ImportHost => ({
    label,
    hostname,
    port: 22,
    username: user,
    group_path: [panel, group],
    tags,
    source: `control:${new URL(request.form.url.trim()).origin}#${id}`,
    exists,
  });
  const hosts = [
    node(1, "edge-hk-01", "hk1.example.com", "Edge", ["prod", "hk"]),
    node(2, "prod-web-01", "10.0.1.11", "Web", ["prod"], true),
    node(3, "edge-sg-01", "192.0.2.31", "Edge", ["prod", "sg"]),
    node(4, "relay-us-01", "relay-us.example.com", "AnixOps Control", []),
    node(5, "relay-eu-01", "198.51.100.7", "AnixOps Control", ["staging"]),
  ];
  return {
    format: null,
    detected: null,
    hosts,
    groups: [[panel], [panel, "Edge"], [panel, "Web"], [panel, "AnixOps Control"]],
    warnings: ["2 node(s) are not approved in the panel (pending or rejected) and were skipped."],
    skipped: 2,
    removed: ["old-sg-02"],
  };
}

/** A canned preview of a JumpServer import (gateway or direct). */
function previewJumpServer(request: ServiceRequest): HostsImportPreviewResult {
  const url = new URL(request.form.url.trim());
  const { form } = request;
  const gateway = form.gateway;
  const asset = (id: number, label: string, address: string, path: string[], port = 22): ImportHost => ({
    label,
    hostname: gateway ? form.gateway_host.trim() || url.hostname : address,
    port: gateway ? form.gateway_port || 2222 : port,
    username: (gateway ? form.gateway_user : form.default_user).trim() || null,
    group_path: [url.hostname, ...path],
    notes: gateway ? `JumpServer asset ${label} (${address}:${port}). Through the KoKo gateway: at the menu type /${label} and press Enter.` : "",
    source: ["jumpserver:" + url.origin, "00000000-0000-0000-0000-000000000002", `a${id}`].join("#"),
    exists: id === 2,
  });
  const hosts = [
    asset(1, "OVH-FR-web", "198.51.100.10", ["Prod", "Web"]),
    asset(2, "prod-web-01", "10.0.1.11", ["Prod", "Web"]),
    asset(3, "HK-edge-01", "192.0.2.31", ["Prod", "Edge"], 2200),
    asset(4, "build-01", "192.0.2.40", ["CI"]),
  ];
  return {
    format: null,
    detected: null,
    hosts,
    groups: [[url.hostname], [url.hostname, "Prod"], [url.hostname, "Prod", "Web"], [url.hostname, "Prod", "Edge"], [url.hostname, "CI"]],
    warnings: [
      "1 asset(s) are disabled in JumpServer and were skipped.",
      "1 asset(s) have no SSH protocol (for example Windows over RDP) and were skipped: win-rdp-01 (Windows).",
    ],
    skipped: 2,
    removed: [],
  };
}

/** Applies an import to the sample data, with the real policies. */
export function previewApplyImport(request: HostsImportApplyParams): HostsImportApplyResult {
  const result: HostsImportApplyResult = { created: 0, updated: 0, unchanged: 0, groups_created: 0, rejected: [], warnings: [] };
  const groupFor = (path: string[] | undefined): string | null => {
    if (request.group_mode === "none") return null;
    if (request.group_mode === "existing") return request.group_id ?? null;
    const name = path?.at(-1);
    if (!name) return null;
    let group = store.groups.find((candidate) => candidate.name.toLowerCase() === name.toLowerCase());
    if (!group) {
      group = { id: `group-${nextId++}`, name, color: null };
      store.groups.push(group);
      result.groups_created += 1;
    }
    return group.id;
  };
  for (const imported of request.hosts) {
    const fields: HostFields = {
      alias: null,
      hostname: imported.hostname,
      username: imported.username ?? null,
      port: imported.port,
      proxyJump: null,
      notes: "",
      shellIntegration: false,
    };
    const match = store.hosts.find(
      (host) =>
        host.source === "saved" &&
        host.fields.hostname.toLowerCase() === imported.hostname.toLowerCase() &&
        host.fields.port === imported.port &&
        host.fields.username === (imported.username ?? null),
    );
    if (match && request.duplicates === "skip") {
      result.unchanged += 1;
    } else if (match && request.duplicates === "update") {
      const groupId = groupFor(imported.group_path);
      Object.assign(match, makeHost(match.id, imported.label, fields, request.group_mode === "none" ? match.groupId : groupId, { favorite: match.favorite, color: match.color ?? undefined }));
      result.updated += 1;
    } else {
      store.hosts.push(makeHost(String(nextId++), imported.label, fields, groupFor(imported.group_path)));
      result.created += 1;
    }
  }
  return result;
}
