export interface MicrosoftAccount { tenantId: string; id: string; displayName: string; email: string }
export interface MicrosoftAuthConfig { tenantId: string; clientId: string }
export interface MicrosoftFolderBinding { localFolder: string; driveId: string; folderId: string; webUrl: string; tenantId: string }
export interface IntakeAttribution {
  path: string; filename: string; state: 'verified' | 'other' | 'unknown' | 'processed' | 'filed';
  reason: string; uploader: MicrosoftAccount | null; processedBy: MicrosoftAccount | null;
  filedAs: string | null; checkedAt: number;
}
export interface MicrosoftIntakeStatus {
  connected: boolean; account: MicrosoftAccount | null; tenantId: string; clientId: string;
  binding: MicrosoftFolderBinding | null; documents: IntakeAttribution[]; error: string | null;
}
export interface MicrosoftDevicePrompt { userCode: string; verificationUri: string; intervalSeconds: number; expiresAt: number }
export type MicrosoftSignInProgress = { state: 'pending'; intervalSeconds: number } | { state: 'connected'; account: MicrosoftAccount };
export interface MicrosoftIntakeBridge {
  microsoftIntakeStatus(): Promise<MicrosoftIntakeStatus>;
  microsoftSignInStart(config: MicrosoftAuthConfig, acknowledgeAuditAccess: boolean): Promise<MicrosoftDevicePrompt>;
  microsoftSignInPoll(): Promise<MicrosoftSignInProgress>;
  microsoftDisconnect(): Promise<void>;
  microsoftBindIntake(driveId: string, folderId: string): Promise<MicrosoftFolderBinding>;
  microsoftOpenSignIn(): Promise<void>;
}
export function validMicrosoftId(value: string): boolean {
  return /^[a-f\d]{8}-[a-f\d]{4}-[a-f\d]{4}-[a-f\d]{4}-[a-f\d]{12}$/i.test(value);
}
