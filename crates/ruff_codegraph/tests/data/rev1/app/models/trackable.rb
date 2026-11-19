module Trackable
  extend ActiveSupport::Concern

  included do
    before_save :track_change
  end
end
