import type { SessionEvent, SessionHandle } from "./backend";
import { previewSnapshot } from "./preview";
import { formatTarget, parseAdhocId } from "./quickConnect";

const ESC = "\x1b";
const CRLF = "\r\n";
const color = (code: string, text: string) => `${ESC}[${code}m${text}${ESC}[0m`;

let previewCount = 0;

/** A fake session with local echo, so the terminal can be previewed and
 *  screenshotted without a backend. `exit` ends it to show that state. */
export function previewSession(
  hostId: string | null,
  onEvent: (event: SessionEvent) => void,
  attach?: "tmux" | "screen",
): SessionHandle {
  const entry = previewSnapshot().hosts.find((candidate) => candidate.id === hostId);
  const adhoc = hostId ? parseAdhocId(hostId) : null;
  const host = entry?.name ?? (adhoc ? formatTarget({ ...adhoc, user: null, port: 22 }) : "local");
  const serial = entry?.fields.serial;
  const user = adhoc?.user ?? (hostId ? "deploy" : "you");
  const prompt = serial
    ? `${host}> `
    : `${color("1;32", `${user}@${host}`)}:${color("1;34", "~")}$ `;
  let line = "";
  let closed = false;
  const emit = (data: string) => {
    if (!closed) onEvent({ kind: "data", data });
  };
  const banner = serial
    ? [
        `${serial.port} at ${serial.baud} baud`,
        "",
        "Switch#1 uptime is 41 weeks, 2 days, 7 hours, 18 minutes",
        color("33", "Press RETURN to get started."),
        "",
      ]
    : hostId
    ? [
        "Welcome to Ubuntu 24.04.1 LTS (GNU/Linux 6.8.0-45-generic x86_64)",
        "",
        "  System load:  0.08              Processes:       142",
        "  Usage of /:   37.2% of 78.12GB  Users logged in: 1",
        "  Memory usage: 41%               IPv4 address:    10.0.1.11",
        "",
        color("33", "3 updates can be applied immediately."),
        "Last login: Tue Oct 14 09:12:03 2026 from 192.168.1.20",
        "",
      ]
    : [];
  window.setTimeout(() => {
    emit(banner.map((text) => text + CRLF).join("") + prompt);
    if (attach) emit(color("2", `[${attach}: anixops]`) + CRLF + prompt);
  }, 120);

  const run = (command: string) => {
    if (command === "exit") {
      emit("logout" + CRLF);
      closed = true;
      onEvent({ kind: "exit", code: 0, reason: "exit" });
      return;
    }
    if (command === "ls") {
      const entries = [color("1;34", "app"), color("1;34", "logs"), "deploy.sh", color("32", "start.sh"), "README.md"];
      emit(entries.join("  ") + CRLF);
    } else if (command === "df -h") {
      const rows = [
        "Filesystem      Size  Used Avail Use% Mounted on",
        "/dev/vda1        78G   29G   46G  39% /",
        "tmpfs           3.9G     0  3.9G   0% /dev/shm",
        "/dev/vdb1       200G  141G   60G  71% /data",
      ];
      emit(rows.join(CRLF) + CRLF);
    } else if (command) {
      emit(`${command.split(" ")[0]}: command not found${CRLF}`);
    }
    emit(prompt);
  };

  return {
    id: `preview-${++previewCount}`,
    engine: hostId ? (hostId === "1" ? "russh" : "openssh") : null,
    engineReason: hostId ? "preview data" : null,
    write(data) {
      for (const char of data) {
        if (char === "\r") {
          emit(CRLF);
          run(line.trim());
          line = "";
        } else if (char === "\x7f") {
          if (line) {
            line = line.slice(0, -1);
            emit("\b \b");
          }
        } else if (char >= " ") {
          line += char;
          emit(char);
        }
      }
    },
    resize() {},
    // The mobile app replaces the connection of an attached session; the
    // preview shows the status events and that the screen is still there.
    reconnect() {
      if (closed || !attach) return;
      onEvent({ kind: "reconnecting", attempt: 1, delay_ms: 0 });
      window.setTimeout(() => {
        if (closed) return;
        onEvent({ kind: "resumed" });
        emit(color("2", `[${attach}: anixops]`) + CRLF + prompt);
      }, 200);
    },
    detach() {
      closed = true;
    },
    close() {
      closed = true;
    },
  };
}
