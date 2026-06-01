import { WsClient } from './client';

let instance: WsClient | null = null;

export function useWebSocket(): WsClient {
  if (!instance) {
    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const url = `${protocol}//${window.location.host}/ws/browser`;
    instance = new WsClient(url);
  }
  return instance;
}
