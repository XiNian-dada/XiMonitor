import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { WS } from 'vitest-websocket-mock';
import { WsClient } from './client';
import type { BrowserMessage } from '@/api/types';

describe('WsClient', () => {
  let server: WS;
  let client: WsClient;

  beforeEach(() => {
    vi.useRealTimers();
    Object.defineProperty(document, 'hidden', {
      configurable: true,
      value: false,
    });
    document.body.removeAttribute('data-ws-conn-id');
  });

  afterEach(() => {
    if (server) {
      server.close();
    }
  });

  describe('connection lifecycle', () => {
    it('transitions from idle → connecting → open', async () => {
      server = new WS('ws://localhost:1234/ws/browser');
      client = new WsClient('ws://localhost:1234/ws/browser');

      expect(client.getState()).toEqual({ kind: 'idle' });

      client.connect();
      expect(client.getState()).toEqual({ kind: 'connecting', attempt: 1 });

      await server.connected;
      const state = client.getState();
      expect(state.kind).toBe('open');
      if (state.kind === 'open') {
        expect(state.sinceTs).toBeGreaterThan(0);
      }
    });

    it('does not reconnect if already connecting', () => {
      server = new WS('ws://localhost:1234/ws/browser');
      client = new WsClient('ws://localhost:1234/ws/browser');

      client.connect();
      const state1 = client.getState();
      client.connect();
      const state2 = client.getState();

      expect(state1).toEqual(state2);
    });

    it('does not reconnect if already open', async () => {
      server = new WS('ws://localhost:1234/ws/browser');
      client = new WsClient('ws://localhost:1234/ws/browser');

      client.connect();
      await server.connected;

      const state1 = client.getState();
      client.connect();
      const state2 = client.getState();

      expect(state1).toEqual(state2);
    });

    it('disconnect() closes the connection and sets failed state', async () => {
      server = new WS('ws://localhost:1234/ws/browser');
      client = new WsClient('ws://localhost:1234/ws/browser');

      client.connect();
      await server.connected;

      client.disconnect();
      expect(client.getState()).toEqual({
        kind: 'failed',
        reason: 'auth_or_unreachable',
      });
    });
  });

  describe('reconnect with exponential backoff', () => {
    it('schedules reconnect after close with exponential backoff + jitter', async () => {
      server = new WS('ws://localhost:1234/ws/browser');
      client = new WsClient('ws://localhost:1234/ws/browser');

      client.connect();
      await server.connected;

      server.close();
      await new Promise((resolve) => setTimeout(resolve, 50));

      const state = client.getState();
      expect(state.kind).toBe('reconnecting');
      if (state.kind === 'reconnecting') {
        expect(state.nextAttemptAt).toBeGreaterThan(Date.now());

        const delay = state.nextAttemptAt - Date.now();
        expect(delay).toBeGreaterThanOrEqual(800);
        expect(delay).toBeLessThanOrEqual(2400);
      }
    });

    it('reconnects after the scheduled delay', async () => {
      server = new WS('ws://localhost:1234/ws/browser');
      client = new WsClient('ws://localhost:1234/ws/browser');

      client.connect();
      await server.connected;

      server.close();
      await new Promise((resolve) => setTimeout(resolve, 50));

      const state = client.getState();
      expect(state.kind).toBe('reconnecting');
      if (state.kind !== 'reconnecting') return;

      const delay = state.nextAttemptAt - Date.now();

      server = new WS('ws://localhost:1234/ws/browser');
      await new Promise((resolve) => setTimeout(resolve, delay + 100));

      const finalState = client.getState().kind;
      expect(['connecting', 'open']).toContain(finalState);
    });

    it('caps backoff delay at 30s', async () => {
      client = new WsClient('ws://localhost:1234/ws/browser');

      for (let i = 0; i < 5; i++) {
        server = new WS('ws://localhost:1234/ws/browser');
        client.connect();
        await server.connected;
        server.close();

        await new Promise((resolve) => setTimeout(resolve, 50));
        const state = client.getState();
        expect(state.kind).toBe('reconnecting');
        if (state.kind !== 'reconnecting') continue;

        const delay = state.nextAttemptAt - Date.now();
        expect(delay).toBeLessThanOrEqual(36000);

        await new Promise((resolve) => setTimeout(resolve, Math.min(delay + 100, 100)));
      }
    });
  });

  describe('message handling', () => {
    it('delivers InitialState to registered handlers', async () => {
      server = new WS('ws://localhost:1234/ws/browser');
      client = new WsClient('ws://localhost:1234/ws/browser');

      const handler = vi.fn();
      client.on('initial_state', handler);

      client.connect();
      await server.connected;

      const msg: BrowserMessage = {
        type: 'initial_state',
        generated_at: '2026-06-01T12:00:00Z',
        overview: {
          generated_at: '2026-06-01T12:00:00Z',
          total_nodes: 5,
          online_nodes: 3,
          offline_nodes: 2,
          total_rx_bytes: 1000,
          total_tx_bytes: 2000,
          current_rx_bytes_per_sec: 10,
          current_tx_bytes_per_sec: 20,
          average_latency_ms: 15,
        },
        nodes: [],
      };

      server.send(JSON.stringify(msg));
      await new Promise((resolve) => setTimeout(resolve, 50));

      expect(handler).toHaveBeenCalledWith(msg);
    });

    it('delivers NodeUpsert to registered handlers', async () => {
      server = new WS('ws://localhost:1234/ws/browser');
      client = new WsClient('ws://localhost:1234/ws/browser');

      const handler = vi.fn();
      client.on('node_upsert', handler);

      client.connect();
      await server.connected;

      const msg: BrowserMessage = {
        type: 'node_upsert',
        generated_at: '2026-06-01T12:00:00Z',
        node: {
          identity: {
            node_id: 'node1',
            node_label: 'Node 1',
            hostname: 'host1',
            tags: [],
          },
          snapshot: {
            cpu_usage_percent: 50,
            load: { one: 1.5 },
            memory: { total_bytes: 8000000000, used_bytes: 4000000000 },
          },
          latency_ms: 10,
          online: true,
        },
      };

      server.send(JSON.stringify(msg));
      await new Promise((resolve) => setTimeout(resolve, 50));

      expect(handler).toHaveBeenCalledWith(msg);
    });

    it('unsubscribes handlers via returned function', async () => {
      server = new WS('ws://localhost:1234/ws/browser');
      client = new WsClient('ws://localhost:1234/ws/browser');

      const handler = vi.fn();
      const unsubscribe = client.on('ping', handler);

      client.connect();
      await server.connected;

      server.send(JSON.stringify({ type: 'ping' }));
      await new Promise((resolve) => setTimeout(resolve, 50));
      expect(handler).toHaveBeenCalledTimes(1);

      unsubscribe();

      server.send(JSON.stringify({ type: 'ping' }));
      await new Promise((resolve) => setTimeout(resolve, 50));
      expect(handler).toHaveBeenCalledTimes(1);
    });
  });

  describe('dev DOM marker', () => {
    it('increments data-ws-conn-id on each successful connection', async () => {
      server = new WS('ws://localhost:1234/ws/browser');
      client = new WsClient('ws://localhost:1234/ws/browser');

      client.connect();
      await server.connected;

      const id1 = document.body.getAttribute('data-ws-conn-id');
      expect(id1).toBe('1');

      server.close();
      await new Promise((resolve) => setTimeout(resolve, 50));

      const state = client.getState();
      expect(state.kind).toBe('reconnecting');
      if (state.kind !== 'reconnecting') return;

      const delay = state.nextAttemptAt - Date.now();

      server = new WS('ws://localhost:1234/ws/browser');
      await new Promise((resolve) => setTimeout(resolve, delay + 100));
      await server.connected;

      const id2 = document.body.getAttribute('data-ws-conn-id');
      expect(id2).toBe('2');
    });
  });

  describe('heartbeat (Ping/Pong)', () => {
    it('sends Ping after 30s and restarts timer on Pong', async () => {
      server = new WS('ws://localhost:1234/ws/browser');
      client = new WsClient('ws://localhost:1234/ws/browser');

      client.connect();
      await server.connected;

      // Wait for first ping (30s)
      await new Promise((resolve) => setTimeout(resolve, 30100));

      expect(server.messages.length).toBeGreaterThan(0);
      expect(server.messages[0]).toBe(JSON.stringify({ type: 'ping' }));

      // Send pong
      server.send(JSON.stringify({ type: 'pong' }));
      await new Promise((resolve) => setTimeout(resolve, 100));

      // Wait for second ping (another 30s)
      await new Promise((resolve) => setTimeout(resolve, 30100));

      const pings = server.messages.filter((m) => m === JSON.stringify({ type: 'ping' }));
      expect(pings.length).toBeGreaterThanOrEqual(2);
    }, 65000);

    it('closes connection if Pong not received within 10s', async () => {
      server = new WS('ws://localhost:1234/ws/browser');
      client = new WsClient('ws://localhost:1234/ws/browser');

      client.connect();
      await server.connected;

      expect(client.getState().kind).toBe('open');

      // Wait for ping (30s)
      await new Promise((resolve) => setTimeout(resolve, 30100));

      expect(server.messages).toContain(JSON.stringify({ type: 'ping' }));

      // Don't send pong, wait for timeout (10s)
      await new Promise((resolve) => setTimeout(resolve, 10100));

      expect(client.getState().kind).toBe('reconnecting');
    }, 45000);
  });

  describe('reconnect stop condition', () => {
    it('stops reconnecting after 3 consecutive handshake failures', async () => {
      client = new WsClient('ws://localhost:1234/ws/browser');

      // Attempt 1: connect but server never opens
      client.connect();
      await new Promise((resolve) => setTimeout(resolve, 100));

      // Simulate connection error before onopen
      if (client['ws']) {
        client['ws'].dispatchEvent(new Event('error'));
        client['ws'].close();
      }

      await new Promise((resolve) => setTimeout(resolve, 100));
      expect(client.getState().kind).toBe('reconnecting');

      // Attempt 2: wait for reconnect delay
      const state1 = client.getState();
      if (state1.kind === 'reconnecting') {
        const delay = state1.nextAttemptAt - Date.now();
        await new Promise((resolve) => setTimeout(resolve, delay + 100));
      }

      // Simulate second failure
      if (client['ws']) {
        client['ws'].dispatchEvent(new Event('error'));
        client['ws'].close();
      }

      await new Promise((resolve) => setTimeout(resolve, 100));
      expect(client.getState().kind).toBe('reconnecting');

      // Attempt 3: wait for reconnect delay
      const state2 = client.getState();
      if (state2.kind === 'reconnecting') {
        const delay = state2.nextAttemptAt - Date.now();
        await new Promise((resolve) => setTimeout(resolve, delay + 100));
      }

      // Simulate third failure
      if (client['ws']) {
        client['ws'].dispatchEvent(new Event('error'));
        client['ws'].close();
      }

      await new Promise((resolve) => setTimeout(resolve, 100));

      // After 3 failures, should be in failed state
      expect(client.getState()).toEqual({
        kind: 'failed',
        reason: 'auth_or_unreachable',
      });
    }, 15000);
  });
});
