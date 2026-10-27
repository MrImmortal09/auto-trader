import React from 'react';
import { AuthProvider, useAuth } from '../context/AuthContext';
import { AdminSignInScreen } from './AdminSignInScreen';

function AuthModalOverlay() {
  const { isUnlockModalOpen, closeUnlockModal, unlockReason } = useAuth();
  if (!isUnlockModalOpen) return null;
  return <AdminSignInScreen reason={unlockReason} onClose={closeUnlockModal} />;
}

/**
 * The dashboard is the owner's: its data (positions, trades, logs,
 * settings) is served only to the admin's sign-in, made on the main page, so
 * without it the pointer there is the whole page. A token the server rejects
 * (expired, or the secret changed) drops write access, which brings the
 * screen back and remounts the dashboard fresh once signed in again.
 */
function Gate({ children }: { children: React.ReactNode }) {
  const { hasWriteAccess, unlockReason } = useAuth();
  if (!hasWriteAccess) return <AdminSignInScreen reason={unlockReason} />;
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
