import { describe, expect, it } from 'vitest';
import {
  frameToCompose,
  hexFromDump,
  hexFromText,
  isHandshakeRequest,
  isSocketUrl,
  isWholeFrame,
  looksLikeHexDump,
  requestPath,
  socketAddress,
} from './wsframe.js';

// A dump in the exact shape the Rust side writes it: `format!("{:08x}  {:<47}  {ascii}")`.
const dump = (bytes) =>
  bytes
    .reduce((rows, b, i) => ((rows[Math.floor(i / 16)] ??= []).push(b), rows), [])
    .map((row, i) => {
      const hex = row.map((b) => b.toString(16).padStart(2, '0')).join(' ');
      const ascii = row.map((b) => (b >= 0x20 && b < 0x7f ? String.fromCharCode(b) : '.')).join('');
      return `${(i * 16).toString(16).padStart(8, '0')}  ${hex.padEnd(47, ' ')}  ${ascii}`;
    })
    .join('\n');

describe('reusing a captured frame', () => {
  it('reads a hex dump back as the bytes it shows', () => {
    const bytes = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    expect(looksLikeHexDump(dump(bytes))).toBe(true);
    expect(hexFromDump(dump(bytes))).toBe('89 50 4e 47 0d 0a 1a 0a');
  });

  it('keeps the ascii column out of the bytes', () => {
    // "de ad be ef" with an ascii column that is itself full of hex letters: a dump
    // read by regex alone would swallow "dead" from the right-hand side.
    const bytes = [...'deadbeefcafe'].map((c) => c.charCodeAt(0));
    expect(hexFromDump(dump(bytes)).split(' ')).toHaveLength(bytes.length);
  });

  it('reads a dump of more than one row', () => {
    const bytes = Array.from({ length: 40 }, (_, i) => i);
    expect(hexFromDump(dump(bytes)).split(' ')).toHaveLength(40);
  });

  it('turns a text frame into text and a binary one into hex', () => {
    expect(frameToCompose({ opcode: 1, text: '{"a":1}' })).toEqual({ kind: 'text', payload: '{"a":1}' });
    expect(frameToCompose({ opcode: 2, text: 'hi' })).toEqual({ kind: 'binary', payload: '68 69' });
    expect(frameToCompose({ opcode: 9, text: 'beat' })).toEqual({ kind: 'ping', payload: 'beat' });
    expect(frameToCompose({ opcode: 10, text: '' })).toEqual({ kind: 'pong', payload: '' });
  });

  it('refuses a close frame and a continuation', () => {
    // "close 1000 bye" is the socket ending, not a message anyone can replay.
    expect(frameToCompose({ opcode: 8, text: 'close 1000 bye' })).toBeNull();
    expect(frameToCompose({ opcode: 0, text: 'x' })).toBeNull();
    expect(frameToCompose(null)).toBeNull();
  });

  it('encodes multi-byte text as the bytes that went over the wire', () => {
    expect(hexFromText('✓')).toBe('e2 9c 93');
  });

  it('knows when what it has is only the start of the frame', () => {
    expect(isWholeFrame({ opcode: 1, text: 'hello', length: 5 })).toBe(true);
    expect(isWholeFrame({ opcode: 1, text: 'hel', length: 500000 })).toBe(false);
    expect(isWholeFrame({ opcode: 2, text: dump([1, 2, 3]), length: 3 })).toBe(true);
    expect(isWholeFrame({ opcode: 2, text: dump([1, 2, 3]), length: 9000 })).toBe(false);
  });
});

describe('recognising a handshake and the socket behind it', () => {
  const head = 'GET /ws/jumbo?kb=256 HTTP/1.1\r\nHost: 127.0.0.1:6677\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n';

  it('reads the upgrade out of the head, CRLF and all', () => {
    expect(isHandshakeRequest(head)).toBe(true);
    expect(isHandshakeRequest(head.replace(/\r\n/g, '\n'))).toBe(true);
    expect(isHandshakeRequest('GET / HTTP/1.1\nupgrade:  WebSocket\n\n')).toBe(true);
  });

  it('is not fooled by a body that talks about websockets', () => {
    // The head ends at the blank line; everything after it is content.
    expect(isHandshakeRequest('POST /log HTTP/1.1\r\nHost: h\r\n\r\nUpgrade: websocket\r\n')).toBe(false);
    expect(isHandshakeRequest('GET / HTTP/1.1\r\nHost: h\r\nUpgrade: h2c\r\n\r\n')).toBe(false);
    expect(isHandshakeRequest('')).toBe(false);
  });

  it('knows an address only a socket can use', () => {
    expect(['ws://h/x', 'WSS://h', '  wss://h  '].map(isSocketUrl)).toEqual([true, true, true]);
    expect(['http://h', 'https://h', ''].map(isSocketUrl)).toEqual([false, false, false]);
  });

  it('takes the path, with its query, off the request line', () => {
    expect(requestPath(head)).toBe('/ws/jumbo?kb=256');
    expect(requestPath('GET / HTTP/1.1')).toBe('/');
    // A proxy-style absolute request line is not a path; fall back rather than guess.
    expect(requestPath('GET http://h/x HTTP/1.1')).toBe('/');
    expect(requestPath('')).toBe('/');
  });

  it('names the socket by what the flow was carried over', () => {
    expect(socketAddress('http', '127.0.0.1:6677', '/ws/notify')).toBe('ws://127.0.0.1:6677/ws/notify');
    expect(socketAddress('https', 'api.example.com', '/stream')).toBe('wss://api.example.com/stream');
    expect(socketAddress('wss', 'h', '')).toBe('wss://h/');
  });
});
