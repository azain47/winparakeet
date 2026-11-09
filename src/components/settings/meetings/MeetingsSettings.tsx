import React, { useCallback, useState } from "react";
import { MeetingDetail } from "./MeetingDetail";
import { MeetingList } from "./MeetingList";
import { MeetingSettings } from "./MeetingSettings";
import { RecordingCard } from "./RecordingCard";

interface MeetingsSettingsProps {
  /** Opens the Models page, to download a bigger model. */
  onOpenModels: () => void;
}

/** The Meetings page: record a call, list meetings, open one. */
export const MeetingsSettings: React.FC<MeetingsSettingsProps> = ({
  onOpenModels,
}) => {
  const [openId, setOpenId] = useState<number | null>(null);
  const back = useCallback(() => setOpenId(null), []);

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      {openId === null ? (
        <>
          <RecordingCard />
          <MeetingList onOpen={setOpenId} />
          <MeetingSettings />
        </>
      ) : (
        <MeetingDetail
          meetingId={openId}
          onBack={back}
          onOpenModels={onOpenModels}
        />
      )}
    </div>
  );
};
