import { describe, expect, it } from "vitest";
import type { Host } from "./backend";
import { defaultSerial, enabledUnlockMethods, framing, isSerialPermissionError, serialHosts, serialReady } from "./serialUi";

function host(id: string, serial: boolean, source: Host["source"] = "saved"): Host {
  return {
    id,
    name: id,
    target: "",
    command: "",
    groupId: null,
    tags: [],
    favorite: false,
    color: null,
    source,
    fields: {
      alias: null,
      hostname: "",
      username: null,
      port: 22,
      proxyJump: null,
      notes: "",
      shellIntegration: false,
      serial: serial ? defaultSerial("/dev/ttyUSB0") : null,
    },
  };
}

describe("serial hosts", () => {
  it("defaults to 115200 8N1 without flow control", () => {
    expect(defaultSerial("COM3")).toEqual({
      port: "COM3",
      baud: 115200,
      dataBits: 8,
      parity: "none",
      stopBits: 1,
      flowControl: "none",
    });
    expect(framing(defaultSerial())).toBe("115200 8N1");
    expect(framing({ ...defaultSerial(), baud: 9600, dataBits: 7, parity: "even", stopBits: 2 })).toBe("9600 7E2");
  });

  it("needs a port and a sane speed to be saved", () => {
    expect(serialReady(defaultSerial("/dev/ttyS0"))).toBe(true);
    expect(serialReady(defaultSerial("  "))).toBe(false);
    expect(serialReady({ ...defaultSerial("COM1"), baud: 10 })).toBe(false);
    expect(serialReady({ ...defaultSerial("COM1"), baud: 5_000_000 })).toBe(false);
    expect(serialReady({ ...defaultSerial("COM1"), baud: 9600.5 })).toBe(false);
  });

  it("lists only saved serial hosts for the new-tab menu", () => {
    const hosts = [host("a", false), host("b", true), host("c", true, "ssh_config")];
    expect(serialHosts(hosts).map((entry) => entry.id)).toEqual(["b"]);
  });

  it("recognises the Linux permission error the daemon reports", () => {
    const daemon = "Permission denied opening /dev/ttyUSB0. Add your user to the group that owns the port";
    expect(isSerialPermissionError(daemon)).toBe(true);
    expect(isSerialPermissionError("/dev/ttyS9 does not exist. Is the device plugged in?")).toBe(false);
  });
});

describe("unlock methods", () => {
  it("shows only methods that are available and on, in order", () => {
    const methods = [
      { name: "windows_hello", available: true, enabled: true },
      { name: "touch_id", available: false, enabled: true },
      { name: "other", available: true, enabled: false },
    ];
    expect(enabledUnlockMethods(methods).map((method) => method.name)).toEqual(["windows_hello"]);
  });
});
