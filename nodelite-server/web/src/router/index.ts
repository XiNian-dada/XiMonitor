import { createRouter, createWebHistory, type RouteRecordRaw } from 'vue-router';

const routes: RouteRecordRaw[] = [
  {
    path: '/',
    name: 'dashboard',
    component: () => import('@/views/DashboardView.vue'),
  },
  {
    path: '/nodes/:id',
    name: 'node-detail',
    component: () => import('@/views/NodeDetailView.vue'),
  },
  {
    path: '/settings',
    name: 'settings',
    component: () => import('@/views/SettingsView.vue'),
  },
  {
    path: '/account',
    name: 'account',
    component: () => import('@/views/AccountView.vue'),
  },
  {
    path: '/alerts',
    name: 'alerts',
    component: () => import('@/views/AlertsView.vue'),
  },
];

export const router = createRouter({
  history: createWebHistory(),
  routes,
  scrollBehavior(to, _from, savedPosition) {
    // Restore saved position when using browser back/forward
    if (savedPosition) {
      return savedPosition;
    }
    // Scroll to anchor if present
    if (to.hash) {
      return { el: to.hash, behavior: 'smooth' };
    }
    // Default: scroll to top on route change
    return { top: 0 };
  },
});
