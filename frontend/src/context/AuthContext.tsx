import React, { createContext, useContext, useEffect, useState, useCallback } from 'react';
import { hasWriteAccess as checkWriteAccess, clearToken, onAuthChange } from '../lib/auth';

/**
 * Write access is the admin's sign-in (see `lib/auth`). `openUnlockModal`
 * asks for it: v1 shows "sign in as the admin on the main page"; v2 reads
 * `hasWriteAccess` through its own identity. `lock` signs out.
 */
interface AuthContextType {
  hasWriteAccess: boolean;
  isUnlockModalOpen: boolean;
  unlockReason: string | null;
  openUnlockModal: (reason?: string) => void;
  closeUnlockModal: () => void;
  lock: () => void;
}

const AuthContext = createContext<AuthContextType | null>(null);

export function AuthProvider({ children }: { children: React.ReactNode }) {
  const [hasWriteAccess, setHasWriteAccess] = useState<boolean>(() => checkWriteAccess());
  const [isUnlockModalOpen, setIsUnlockModalOpen] = useState<boolean>(false);
  const [unlockReason, setUnlockReason] = useState<string | null>(null);

  useEffect(() => {
    // Signed in, out, or changed in another tab.
    const unsubscribe = onAuthChange((valid) => {
      setHasWriteAccess(valid);
      if (valid) {
        setIsUnlockModalOpen(false);
        setUnlockReason(null);
      }
    });

    const handleUnauthorizedEvent = (e: Event) => {
      const customEvent = e as CustomEvent<{ isMutation?: boolean }>;
      // The stored token was refused (see lib/api's apiFetch): it is gone,
      // and with it write access.
      setHasWriteAccess(false);
      setIsUnlockModalOpen(true);
      setUnlockReason(
        customEvent.detail?.isMutation
          ? 'Your sign-in has expired. Sign in again as the admin to make changes.'
          : 'Your sign-in has expired. Sign in again as the admin to continue.',
      );
    };

    window.addEventListener('at_unauthorized', handleUnauthorizedEvent);
    return () => {
      unsubscribe();
      window.removeEventListener('at_unauthorized', handleUnauthorizedEvent);
    };
  }, []);

  const openUnlockModal = useCallback((reason?: string) => {
    setUnlockReason(reason || null);
    setIsUnlockModalOpen(true);
  }, []);

  const closeUnlockModal = useCallback(() => {
    setIsUnlockModalOpen(false);
    setUnlockReason(null);
  }, []);

  const lock = useCallback(() => {
    clearToken();
    setHasWriteAccess(false);
  }, []);

  return (
    <AuthContext.Provider
      value={{
        hasWriteAccess,
        isUnlockModalOpen,
        unlockReason,
        openUnlockModal,
        closeUnlockModal,
        lock,
      }}
    >
      {children}
    </AuthContext.Provider>
  );
}

export function useAuth(): AuthContextType {
  const context = useContext(AuthContext);
  if (!context) {
    throw new Error('useAuth must be used within an AuthProvider');
  }
  return context;
}
