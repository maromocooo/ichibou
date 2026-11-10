// Turns a raw H.264 byte stream (Annex B, as `adb screenrecord` writes it)
// into frames for a WebCodecs VideoDecoder.

export interface Frame {
  key: boolean;
  /** Annex B bytes: parameter sets (for key frames) and the picture's slices. */
  data: Uint8Array;
  /** `avc1.PPCCLL`, from the stream's SPS. Present on key frames. */
  codec?: string;
}

const START = new Uint8Array([0, 0, 0, 1]);

function concat(parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}

const hex = (byte: number | undefined) => (byte ?? 0).toString(16).padStart(2, "0");

/** Splits the stream into NAL units and groups them into frames. */
export class H264Parser {
  private buffer: Uint8Array = new Uint8Array(0);
  private sps: Uint8Array | null = null;
  private pps: Uint8Array | null = null;
  /** Slices of the picture being assembled. */
  private picture: Uint8Array[] = [];
  private pictureIsKey = false;

  /** Feeds bytes; returns the frames that are now complete. */
  push(chunk: Uint8Array): Frame[] {
    this.buffer = concat([this.buffer, chunk]);
    const frames: Frame[] = [];
    // A unit ends where the next start code begins, so the last one in the
    // buffer stays there until more data (or `flush`) arrives.
    let start = this.findStart(0);
    while (start !== null) {
      const next = this.findStart(start.payload);
      if (next === null) break;
      this.unit(this.buffer.subarray(start.payload, next.at), frames);
      start = next;
    }
    if (start !== null) this.buffer = this.buffer.slice(start.at);
    else if (this.buffer.length > 3) this.buffer = this.buffer.slice(this.buffer.length - 3);
    return frames;
  }

  /**
   * Call when the stream has been quiet for a moment: the recorder only
   * writes when the screen changes, so what is buffered is a whole unit and
   * the picture it ends should be shown now, not when the next one arrives.
   */
  flush(): Frame[] {
    const frames: Frame[] = [];
    const start = this.findStart(0);
    if (start !== null && this.buffer.length > start.payload) {
      this.unit(this.buffer.subarray(start.payload), frames);
      this.buffer = new Uint8Array(0);
    }
    this.finishPicture(frames);
    return frames;
  }

  private findStart(from: number): { at: number; payload: number } | null {
    const b = this.buffer;
    for (let i = from; i + 2 < b.length; i++) {
      if (b[i] === 0 && b[i + 1] === 0) {
        if (b[i + 2] === 1) return { at: i, payload: i + 3 };
        if (b[i + 2] === 0 && b[i + 3] === 1) return { at: i, payload: i + 4 };
      }
    }
    return null;
  }

  private finishPicture(frames: Frame[]) {
    if (this.picture.length === 0) return;
    const parts: Uint8Array[] = [];
    let codec: string | undefined;
    if (this.pictureIsKey && this.sps && this.pps) {
      parts.push(START, this.sps, START, this.pps);
      codec = `avc1.${hex(this.sps[1])}${hex(this.sps[2])}${hex(this.sps[3])}`;
    }
    for (const slice of this.picture) parts.push(START, slice);
    frames.push({ key: this.pictureIsKey, data: concat(parts), codec });
    this.picture = [];
  }

  private unit(nal: Uint8Array, frames: Frame[]) {
    if (nal.length === 0) return;
    const type = nal[0]! & 0x1f;
    if (type === 7) {
      this.finishPicture(frames);
      this.sps = nal.slice();
    } else if (type === 8) {
      this.finishPicture(frames);
      this.pps = nal.slice();
    } else if (type === 1 || type === 5) {
      // first_mb_in_slice is the first field of the slice header, coded so
      // that a leading 1 bit means 0: this slice starts a new picture.
      const startsPicture = nal.length > 1 && (nal[1]! & 0x80) !== 0;
      if (startsPicture) this.finishPicture(frames);
      if (this.picture.length === 0) this.pictureIsKey = type === 5;
      this.picture.push(nal.slice());
    }
    // Other unit types (SEI, access unit delimiters) carry no picture data.
  }
}
