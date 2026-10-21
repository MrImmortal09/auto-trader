import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import './styles.css';
import App from './App';
import { AuthProvider } from '../context/AuthContext';
import { ToastProvider } from './ui/Toasts';

// The redesign at /v2/. It shares v1's login (the passkey token and server
// URL in localStorage) but none of v1's components or styles.
createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <AuthProvider>
      <ToastProvider>
        <App />
      </ToastProvider>
    </AuthProvider>
  </StrictMode>,
);
