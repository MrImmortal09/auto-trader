import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import './styles.css';
import App from './App';
import { AuthProvider } from '../context/AuthContext';
import { IdentityProvider } from './auth/IdentityProvider';
import { ToastProvider } from './ui/Toasts';

// The redesign at /v2/. It shares v1's login (the passkey token and server
// URL in localStorage) but none of v1's components or styles. The phone
// sign-in (`IdentityProvider`) is v2's own.
createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <AuthProvider>
      <IdentityProvider>
        <ToastProvider>
          <App />
        </ToastProvider>
      </IdentityProvider>
    </AuthProvider>
  </StrictMode>,
);
