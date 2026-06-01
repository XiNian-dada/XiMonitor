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

  private pingTimer: ReturnType<typeof setTimeout> | null = null;
  private pongTimer: ReturnType<typeof setTimeout> | null = null;

  private readonly handlers = new Map<string, Set<MessageHandler<BrowserMessage['type']>>>();

  constructor(private readonly url: string) {
    this.handleVisibilityChange = this.handleVisibilityChange.bind(this);
    if (typeof document !== 'undefined') {
      document.addEventListener('visibilitychange', this.handleVisibilityChange);
    }
  }

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
    this.clearHeartbeat();
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
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    this.handlers.get(type)!.add(handler as any);

    return () => {
      const set = this.handlers.get(type);
      if (set) {
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        set.delete(handler as any);
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

    // Dev-only DOM marker for E2E tests
    if (process.env.NODE_ENV !== 'production') {
      document.body.setAttribute('data-ws-conn-id', String(this.connectionId));
    }

    this.startHeartbeat();
    console.log('WebSocket connected');
  }

  private onMessage(event: MessageEvent): void {
    try {
      const msg = JSON.parse(event.data) as BrowserMessage;

      if (msg.type === 'pong') {
        this.clearPongTimer();
        return;
      }

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
      this.probeAuthState();
    }
  }

  private onClose(event: CloseEvent): void {
    console.log('WebSocket closed', event.code, event.reason);
    this.ws = null;
    this.clearHeartbeat();

    if (this.state.kind === 'failed') return;

    if (this.handshakeFailures >= 3) {
      this.setState({ kind: 'failed', reason: 'auth_or_unreachable' });
      return;
    }

    // Don't reconnect if tab is hidden — visibility handler will resume on show
    if (typeof document !== 'undefined' && document.hidden) {
      this.setState({ kind: 'idle' });
      return;
    }

    this.scheduleReconnect();
  }

  private probeAuthState(): void {
    fetch('/api/bootstrap', {
      credentials: 'same-origin',
      redirect: 'follow',
    })
      .then((res) => {
        // fetch() follows 302 redirects, so check res.redirected + pathname
        if (res.redirected && new URL(res.url).pathname === '/verify-2fa') {
          console.warn('Auth probe detected 2FA required, navigating to verify-2fa');
          window.location.href = '/verify-2fa';
          return;
        }
        if (res.status === 401) {
          console.warn('Auth probe detected 401, navigating to logout-and-reauth');
          window.location.href = '/logout-and-reauth';
        }
      })
      .catch((err) => {
        console.error('Auth probe failed', err);
      });
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

  private startHeartbeat(): void {
    this.clearHeartbeat();
    this.pingTimer = setTimeout(() => {
      if (this.ws && this.ws.readyState === WebSocket.OPEN) {
        this.ws.send(JSON.stringify({ type: 'ping' }));
        this.pongTimer = setTimeout(() => {
          console.warn('Pong timeout, closing connection');
          if (this.ws) this.ws.close();
        }, 10000);
      }
    }, 30000);
  }

  private clearPongTimer(): void {
    if (this.pongTimer !== null) {
      clearTimeout(this.pongTimer);
      this.pongTimer = null;
    }
    this.startHeartbeat();
  }

  private clearHeartbeat(): void {
    if (this.pingTimer !== null) {
      clearTimeout(this.pingTimer);
      this.pingTimer = null;
    }
    if (this.pongTimer !== null) {
      clearTimeout(this.pongTimer);
      this.pongTimer = null;
    }
  }

  private handleVisibilityChange(): void {
    if (typeof document === 'undefined') return;

    if (document.hidden) {
      if (this.ws && this.ws.readyState === WebSocket.OPEN) {
        console.log('Tab hidden, closing WebSocket');
        this.ws.close();
      }
    } else {
      if (this.state.kind === 'failed' || this.state.kind === 'idle' || this.state.kind === 'reconnecting') {
        console.log('Tab visible, reconnecting');
        this.clearReconnectTimer();
        this.handshakeFailures = 0;
        this.reconnectAttempt = 0;
        this.connect();
      }
    }
  }

  destroy(): void {
    if (typeof document !== 'undefined') {
      document.removeEventListener('visibilitychange', this.handleVisibilityChange);
    }
    this.disconnect();
  }
}
