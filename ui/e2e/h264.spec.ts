// The H.264 stream parser, checked against hand-built byte streams.
import { expect, test } from "@playwright/test";
import { H264Parser } from "../src/h264";

const start4 = [0, 0, 0, 1];
const start3 = [0, 0, 1];
const sps = [0x67, 0x42, 0xc0, 0x29, 0xaa];
const pps = [0x68, 0xce, 0x3c, 0x80];
// The byte after the header starts with a 1 bit: first slice of a picture.
const idr = [0x65, 0x88, 1, 2, 3];
const p1 = [0x41, 0x9a, 4, 5];
const p2 = [0x41, 0x9a, 6, 7];
// A leading 0 bit: a further slice of the same picture.
const p2more = [0x41, 0x1a, 8, 9];
const bytes = (...parts: number[][]) => new Uint8Array(parts.flat());

test("groups units into frames and names the codec from the SPS", () => {
  const parser = new H264Parser();
  const frames = [
    ...parser.push(bytes(start4, sps, start4, pps, start4, idr, start4, p1, start3, p2, start4, p2more)),
    ...parser.flush(),
  ];
  expect(frames.map((f) => f.key)).toEqual([true, false, false]);
  expect(frames[0]!.codec).toBe("avc1.42c029");
  // A key frame carries its parameter sets, so a decoder can start there.
  expect([...frames[0]!.data]).toEqual([...start4, ...sps, ...start4, ...pps, ...start4, ...idr]);
  expect([...frames[1]!.data]).toEqual([...start4, ...p1]);
  // Two slices of one picture stay together.
  expect([...frames[2]!.data]).toEqual([...start4, ...p2, ...start4, ...p2more]);
});

test("gives the same frames however the bytes are split", () => {
  const stream = bytes(start4, sps, start4, pps, start4, idr, start4, p1, start4, p2);
  const whole = new H264Parser();
  const expected = [...whole.push(stream), ...whole.flush()];
  for (const size of [1, 2, 3, 5, 7]) {
    const parser = new H264Parser();
    const frames = [];
    for (let at = 0; at < stream.length; at += size) frames.push(...parser.push(stream.subarray(at, at + size)));
    frames.push(...parser.flush());
    expect(frames.map((f) => [f.key, [...f.data]]), `chunks of ${size}`).toEqual(
      expected.map((f) => [f.key, [...f.data]]),
    );
  }
});

test("holds the last picture until the stream goes quiet", () => {
  const parser = new H264Parser();
  // Nothing can be emitted yet: the picture might have more slices.
  expect(parser.push(bytes(start4, sps, start4, pps, start4, idr))).toEqual([]);
  expect(parser.flush().map((f) => f.key)).toEqual([true]);
  expect(parser.flush()).toEqual([]);
});
