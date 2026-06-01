import type { BrowserMessage } from '@/api/types';

export type ConnectionState =
  | { kind: 'idle' }
  | { kind: 'connecting'; attempt: number }
  | { kind: 'open'; sinceTs: number }
  | { kind: 'reconnecting'; nextAttemptAt: number; attempt: number }
  | { kind: 'failed'; reason: 'auth_or_unreachable' };

type MessageHandler<T extends BrowserMessage['type']> = (
  msg: Extract<BrowserMessage, { type: T }>,
) => void;

type UnsubscribeFn = () => void;

export class WsClient {
  private ws: WebSocket | null = null;
  private state: ConnectionState = { kind: 'idle' };
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectAttempt = 0;
  private connectionId = 0;
  private handshakeFailures = 0;

  private readonly handlers = new Map<string, Set<MessageHandler<BrowserMessage['type']>>>();

  constructor(private readonly url: string) {}

  connect(): void {
    if (this.state.kind === 'connecting' || this.state.kind === 'open') return;

    this.reconnectAttempt++;
    this.setState({ kind: 'connecting', attempt: this.reconnectAttempt });

    try {
      this.ws = new WebSocket(this.url);
      this.ws.onopen = this.onOpen.bind(this);
      this.ws.onmessage = this.onMessage.bind(this);
      this.ws.onerror = this.onError.bind(this);
      this.ws.onclose = this.onClose.bind(this);
    } catch (e) {
      console.error('WebSocket construction failed', e);
      this.scheduleReconnect();
    }
  }

  disconnect(): void {
    this.clearReconnectTimer();
    this.setState({ kind: 'failed', reason: 'auth_or_unreachable' });
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
  }

  on<T extends BrowserMessage['type']>(
    type: T,
    handler: MessageHandler<T>,
  ): UnsubscribeFn {
    if (!this.handlers.has(type)) {
      this.handlers.set(type, new Set());
    }
    this.handlers.get(type)!.add(handler);

    return () => {
      const set = this.handlers.get(type);
      if (set) {
        set.delete(handler);
        if (set.size === 0) this.handlers.delete(type);
      }
    };
  }

  getState(): ConnectionState {
    return this.state;
  }

  private onOpen(): void {
    this.connectionId++;
    this.reconnectAttempt = 0;
    this.handshakeFailures = 0;
    this.setState({ kind: 'open', sinceTs: Date.now() });

    if (import.meta.env.DEV) {
      document.body.setAttribute('data-ws-conn-id', String(this.connectionId));
    }

    console.log('WebSocket connected');
  }

  private onMessage(event: MessageEvent): void {
    try {
      const msg = JSON.parse(event.data) as BrowserMessage;
      const set = this.handlers.get(msg.type);
      if (set) {
        set.forEach((handler) => handler(msg));
      }
    } catch (e) {
      console.error('Failed to parse WebSocket message', e);
    }
  }

  private onError(event: Event): void {
    console.error('WebSocket error', event);
    if (this.state.kind === 'connecting') {
      this.handshakeFailures++;
    }
  }

  private onClose(event: CloseEvent): void {
    console.log('WebSocket closed', event.code, event.reason);
    this.ws = null;

    if (this.state.kind === 'failed') return;

    if (this.handshakeFailures >= 3) {
      this.setState({ kind: 'failed', reason: 'auth_or_unreachable' });
      return;
    }

    this.scheduleReconnect();
  }

  private scheduleReconnect(): void {
    this.clearReconnectTimer();

    const baseDelay = Math.min(1000 * 2 ** this.reconnectAttempt, 30000);
    const jitter = baseDelay * 0.2 * (Math.random() * 2 - 1);
    const delay = Math.max(1000, baseDelay + jitter);
    const nextAttemptAt = Date.now() + delay;

    this.setState({
      kind: 'reconnecting',
      nextAttemptAt,
      attempt: this.reconnectAttempt + 1,
    });

    this.reconnectTimer = setTimeout(() => {
      this.connect();
    }, delay);
  }

  private clearReconnectTimer(): void {
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  }

  private setState(state: ConnectionState): void {
    this.state = state;
  }
}
