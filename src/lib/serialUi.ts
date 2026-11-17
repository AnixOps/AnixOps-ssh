import type { Host, SerialFields } from "./backend";

/** The speeds offered in the host editor; any number from 50 to 4 000 000 is accepted. */
export const BAUD_RATES = [1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600];

export const MIN_BAUD = 50;
export const MAX_BAUD = 4_000_000;

/** 115200 8N1, no flow control: what most devices expect. */
export function defaultSerial(port = ""): SerialFields {
  return { port, baud: 115200, dataBits: 8, parity: "none", stopBits: 1, flowControl: "none" };
}

/** The usual shorthand for a line's framing, such as `115200 8N1`. */
export function framing(serial: SerialFields): string {
  const parity = { none: "N", odd: "O", even: "E" }[serial.parity];
  return `${serial.baud} ${serial.dataBits}${parity}${serial.stopBits}`;
}

/** Whether the editor's serial settings can be saved. */
export function serialReady(serial: SerialFields): boolean {
  return (
    serial.port.trim() !== "" &&
    Number.isInteger(serial.baud) &&
    serial.baud >= MIN_BAUD &&
    serial.baud <= MAX_BAUD
  );
}

/** The saved hosts that are serial ports, for the new-tab menu. */
export function serialHosts(hosts: readonly Host[]): Host[] {
  return hosts.filter((host) => host.source === "saved" && host.fields.serial);
}

/** The daemon's error for a serial port the user may not open (Linux: not in
 *  the `dialout` group). The UI then shows its own advice in the user's language. */
export function isSerialPermissionError(error: string): boolean {
  return error.includes("Permission denied opening");
}

/** Which of the unlock methods are available and turned on, in the order they are tried. */
export function enabledUnlockMethods<T extends { available: boolean; enabled: boolean }>(methods: readonly T[]): T[] {
  return methods.filter((method) => method.available && method.enabled);
}
