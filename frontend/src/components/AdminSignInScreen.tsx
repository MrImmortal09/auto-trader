import { ShieldCheck, X } from 'lucide-react';

/**
 * v1 opens only for the admin's sign-in, which happens on the main page (a
 * phone number and a WhatsApp code). Same site, same browser storage: once
 * signed in there, this app opens too. This screen only points the way.
 */
export function AdminSignInScreen({ reason, onClose }: { reason?: string | null; onClose?: () => void }) {
  return (
    <div className="fixed inset-0 bg-surface/90 backdrop-blur-sm z-50 flex items-center justify-center p-4">
      <div className="relative bg-surface-container-lowest border border-outline-variant rounded-3xl p-8 sm:p-12 shadow-2xl max-w-md w-full flex flex-col items-center text-center">
        {onClose && (
          <button
            type="button"
            onClick={onClose}
            aria-label="Close"
            className="absolute top-4 right-4 p-2 rounded-full text-on-surface-variant hover:text-on-surface hover:bg-surface transition-colors"
          >
            <X size={20} />
          </button>
        )}
        <div className="bg-primary/10 p-4 rounded-full mb-6">
          <ShieldCheck size={48} className="text-primary" />
        </div>
        <h2 className="text-2xl font-bold text-on-surface mb-2">Sign in as the admin</h2>
        <p className="text-on-surface-variant mb-6 text-sm">
          {reason || 'This app opens for the admin only. Sign in with your phone on the main page, then come back here.'}
        </p>
        <a href="/" className="w-full rounded-xl bg-primary text-on-primary font-semibold py-3 hover:bg-primary/90 transition-colors">
          Go to sign in
        </a>
      </div>
    </div>
  );
}
