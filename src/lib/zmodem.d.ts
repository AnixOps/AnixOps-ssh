/* The parts of zmodem.js (Apache-2.0) that terminalTransfer.ts uses. */
declare module "zmodem.js" {
  export interface FileDetails {
    name: string;
    size?: number | null;
    mtime?: Date | null;
    mode?: number | null;
    files_remaining?: number | null;
    bytes_remaining?: number | null;
  }
  export interface Offer {
    get_details(): FileDetails;
    get_offset(): number;
    accept(options: { on_input: (payload: ArrayLike<number>) => void; offset?: number }): Promise<unknown>;
    skip(): void;
  }
  export interface Transfer {
    send(chunk: ArrayLike<number>): void;
    end(chunk?: ArrayLike<number>): Promise<void>;
    get_offset(): number;
  }
  export interface ZSession {
    type: "send" | "receive";
    on(event: "offer", handler: (offer: Offer) => void): this;
    on(event: "session_end", handler: () => void): this;
    start(): Promise<unknown>;
    send_offer(file: FileDetails): Promise<Transfer | undefined>;
    close(): Promise<void>;
    abort(): void;
    has_ended(): boolean;
  }
  export interface Detection {
    confirm(): ZSession;
    deny(): void;
    get_session_role(): "send" | "receive";
  }
  export class Sentry {
    constructor(options: {
      to_terminal: (octets: number[]) => void;
      sender: (octets: number[]) => void;
      on_detect: (detection: Detection) => void;
      on_retract: () => void;
    });
    consume(input: ArrayLike<number> | ArrayBuffer): void;
  }
  const Zmodem: { Sentry: typeof Sentry };
  export default Zmodem;
}
