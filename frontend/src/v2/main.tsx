import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import './styles.css';
import App from './App';
import { IdentityProvider } from './auth/IdentityProvider';
import { SignInGate } from './auth/SignInGate';
import { ToastProvider } from './ui/Toasts';

// The redesign, the site's front page (v1 is at /v1/). It shares the sign-in
// and server URL in localStorage with v1, but none of v1's components or
// styles. Nothing opens without a phone sign-in (`SignInGate`); v1 opens with
// the admin's, made here.
createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <IdentityProvider>
      <ToastProvider>
        <SignInGate>
          <App />
        </SignInGate>
      </ToastProvider>
    </IdentityProvider>
  </StrictMode>,
);
