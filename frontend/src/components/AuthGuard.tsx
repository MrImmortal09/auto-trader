import React from 'react';
import { AuthProvider, useAuth } from '../context/AuthContext';
import { PasskeyScreen } from './PasskeyScreen';

function AuthModalOverlay() {
  const { isUnlockModalOpen, closeUnlockModal, unlockReason, unlock } = useAuth();

  if (!isUnlockModalOpen) return null;

  return (
    <PasskeyScreen
      onSuccess={(token?: string) => {
        if (token) {
          unlock(token);
        } else {
          closeUnlockModal();
        }
      }}
      onClose={closeUnlockModal}
      reason={unlockReason}
    />
  );
}

/**
 * The dashboard is the owner's: its data (positions, trades, logs,
 * settings) is served only to the passkey, so without a valid token the
 * passkey screen is the whole page. A token the server rejects (expired, or
 * the secret changed) drops write access, which brings the screen back and
 * remounts the dashboard fresh once the passkey is entered again.
 */
function Gate({ children }: { children: React.ReactNode }) {
  const { hasWriteAccess, unlock, unlockReason } = useAuth();
  if (!hasWriteAccess) {
    return (
      <PasskeyScreen
        onSuccess={(token?: string) => {
          if (token) unlock(token);
        }}
        reason={unlockReason ?? 'Enter the passkey to open the dashboard.'}
      />
    );
  }
  return (
    <>
      {children}
      <AuthModalOverlay />
    </>
  );
}

export function AuthGuard({ children }: { children: React.ReactNode }) {
  return (
    <AuthProvider>
      <Gate>{children}</Gate>
    </AuthProvider>
  );
}
